//! The declaration table for every built-in authorable component.
//!
//! Each entry states a component's identity, fields, editor controls, and
//! spawn callback exactly once. `builtin_registry()` derives the schemas and
//! Inspector metadata from this table, and the scene bridge derives its
//! per-field value validation from the same declarations, so adding a
//! component means adding one entry here plus its spawn callback.

use super::definition::{
    asset_ref, boolean, entity_ref, enumeration, filtered_entity_ref, float, integer, number, text,
    BuiltinComponent, FieldDef, FieldDefaultSpec, FieldKind,
};
use super::schemas::{authoring_builtin_schema, builtin_asset_id};
use super::{
    AssetKind, InspectorFieldCondition, InspectorFieldControl, NumericRange, NON_NEGATIVE,
    POSITIVE, UNIT_INTERVAL, U32_RANGE,
};
use crate::asset::Assets;
use crate::camera::{Camera3D, OrbitCamera};
use crate::character_controller::KinematicCharacterController;
use crate::combat::DamageReceiver;
use crate::foot_ik::FootIk;
use crate::light::{AmbientLight, DirectionalLight, PointLight, SpotLight};
use crate::lock_on::LockOnTarget;
use crate::mesh::Mesh;
use crate::navmesh::NavMeshAgent;
use crate::particles::ParticleEmitter;
use crate::player::PlayerController;
use crate::postprocess::{PostProcessSettings, ToneMapOperator};
use crate::scene_bridge::*;
use crate::shadow::{EnvironmentLighting, ShadowSettings};
use engine_authoring::value::Value;
use std::collections::BTreeMap;

/// Description shared by every `color_*` field, matching the authoring schema
/// these components have always published.
const COLOR_DESCRIPTION: &str = "Color component in the range 0.0 to 1.0.";

/// Declares a `color_*` component field.
const fn color(name: &'static str, display_name: &'static str, default: fn() -> Value) -> FieldDef {
    FieldDef::new(
        name,
        display_name,
        COLOR_DESCRIPTION,
        FieldKind::F64,
        FieldDefaultSpec::Computed(default),
    )
}

/// Declares an array field holding `element` values, defaulting to empty.
const fn list(
    name: &'static str,
    display_name: &'static str,
    description: &'static str,
    element: &'static FieldKind,
) -> FieldDef {
    FieldDef::new(
        name,
        display_name,
        description,
        FieldKind::Array(element),
        FieldDefaultSpec::Computed(empty_array),
    )
}

/// Declares a free-form string-keyed map field defaulting to empty.
const fn map(name: &'static str, display_name: &'static str, description: &'static str) -> FieldDef {
    FieldDef::new(
        name,
        display_name,
        description,
        FieldKind::Object,
        FieldDefaultSpec::Computed(empty_object),
    )
}

fn empty_array() -> Value {
    Value::Array(Vec::new())
}

fn empty_object() -> Value {
    Value::Object(BTreeMap::new())
}

const ASSET_REF_KIND: FieldKind = FieldKind::AssetRef;
const OBJECT_KIND: FieldKind = FieldKind::Object;
const STRING_KIND: FieldKind = FieldKind::Str;

const STATIC_MESH_RENDERER_FIELDS: &[FieldDef] = &[
    asset_ref(
        "mesh",
        "Mesh",
        "The static mesh asset rendered by this entity.",
        AssetKind::Mesh,
        FieldDefaultSpec::Computed(builtin_triangle),
    ),
    asset_ref(
        "material",
        "Material",
        "The base material used when a submesh has no slot override.",
        AssetKind::Material,
        FieldDefaultSpec::Computed(builtin_white_material),
    ),
    list(
        "material_slots",
        "Material Slots",
        "Optional material overrides indexed by submesh order.",
        &ASSET_REF_KIND,
    )
    .with_control(InspectorFieldControl::AssetRefList(AssetKind::Material)),
];

const SKINNED_MESH_RENDERER_FIELDS: &[FieldDef] = &[
    asset_ref(
        "mesh",
        "Mesh",
        "Imported mesh sub-asset carrying joint and weight attributes.",
        AssetKind::Mesh,
        FieldDefaultSpec::Unassigned,
    ),
    filtered_entity_ref(
        "model",
        "Skinned Model",
        "Skinned Model whose runtime rig deforms this mesh; leave unassigned to keep the renderer in its bind-pose editing state.",
        &[SKINNED_MODEL_COMPONENT],
    )
    .optional(),
    asset_ref(
        "material",
        "Material",
        "The base material used when a submesh has no slot override.",
        AssetKind::Material,
        FieldDefaultSpec::Computed(builtin_white_material),
    ),
    list(
        "material_slots",
        "Material Slots",
        "Optional material overrides indexed by submesh order.",
        &ASSET_REF_KIND,
    )
    .with_control(InspectorFieldControl::AssetRefList(AssetKind::Material)),
];

const SKINNED_MODEL_FIELDS: &[FieldDef] = &[asset_ref(
    "skeleton",
    "Skeleton",
    "Imported Skeleton sub-asset whose bones become this character's rig.",
    AssetKind::Skeleton,
    FieldDefaultSpec::Unassigned,
)];

const BONE_ATTACHMENT_FIELDS: &[FieldDef] = &[
    filtered_entity_ref(
        "rig",
        "Rig",
        "The Skinned Model whose bone this entity follows.",
        &[SKINNED_MODEL_COMPONENT],
    ),
    FieldDef::new(
        "bone",
        "Bone",
        "Stable bone identity within that rig's skeleton asset; picked by name and stored as an ID so a renamed bone still resolves.",
        FieldKind::I64,
        FieldDefaultSpec::I64(-1),
    )
    .with_control(InspectorFieldControl::BoneRef { rig_field: "rig" }),
    text(
        "bone_name",
        "Bone Name",
        "The bone's name when it was picked; shown in the Inspector and in diagnostics, never used to resolve the binding.",
        "",
    ),
];

const LOD_GROUP_FIELDS: &[FieldDef] = &[FieldDef::new(
    "levels",
    "Levels",
    "Strictly increasing positive distance thresholds and their meshes.",
    FieldKind::Array(&OBJECT_KIND),
    FieldDefaultSpec::Computed(default_lod_levels),
)
.with_control(InspectorFieldControl::LodLevels)];

const RIGID_BODY_PHYSICS_FIELDS: &[FieldDef] = &[asset_ref(
    "rig",
    "Rig",
    "The Secondary Motion Rig sub-asset imported from this character's model. Bodies and joints are editable engine-native starting values produced by import.",
    AssetKind::SecondaryMotionRig,
    FieldDefaultSpec::Unassigned,
)];

const CAMERA_FIELDS: &[FieldDef] = &[
    FieldDef::new(
        "enabled",
        "Enabled",
        "Whether this camera may drive the Game View and camera-dependent gameplay.",
        FieldKind::Bool,
        FieldDefaultSpec::Computed(default_camera_enabled),
    ),
    FieldDef::new(
        "priority",
        "Priority",
        "Selection priority among enabled cameras; higher values win.",
        FieldKind::I64,
        FieldDefaultSpec::Computed(default_camera_priority),
    )
    .with_control(InspectorFieldControl::Number(NumericRange::inclusive(
        i32::MIN as f64,
        i32::MAX as f64,
    ))),
    FieldDef::new(
        "fov_y_degrees",
        "FOV Y",
        "Vertical field of view in degrees.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_camera_fov),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
    FieldDef::new(
        "near",
        "Near",
        "Near clipping plane distance.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_camera_near),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
    FieldDef::new(
        "far",
        "Far",
        "Far clipping plane distance.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_camera_far),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
];

const DIRECTIONAL_LIGHT_FIELDS: &[FieldDef] = &[
    FieldDef::new(
        "direction_x",
        "Direction X",
        "X component of the light travel direction.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_sun_direction_x),
    ),
    FieldDef::new(
        "direction_y",
        "Direction Y",
        "Y component of the light travel direction.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_sun_direction_y),
    ),
    FieldDef::new(
        "direction_z",
        "Direction Z",
        "Z component of the light travel direction.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_sun_direction_z),
    ),
    color("color_r", "Color R", default_sun_color_r),
    color("color_g", "Color G", default_sun_color_g),
    color("color_b", "Color B", default_sun_color_b),
    FieldDef::new(
        "intensity",
        "Intensity",
        "Directional light intensity multiplier.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_sun_intensity),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
];

const POINT_LIGHT_FIELDS: &[FieldDef] = &[
    color("color_r", "Color R", default_point_color_r)
        .with_control(InspectorFieldControl::Number(UNIT_INTERVAL)),
    color("color_g", "Color G", default_point_color_g)
        .with_control(InspectorFieldControl::Number(UNIT_INTERVAL)),
    color("color_b", "Color B", default_point_color_b)
        .with_control(InspectorFieldControl::Number(UNIT_INTERVAL)),
    FieldDef::new(
        "intensity",
        "Intensity",
        "Local-light radiant intensity multiplier before distance attenuation.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_point_intensity),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "range",
        "Range",
        "Maximum world-space influence distance.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_point_range),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
];

const SPOT_LIGHT_FIELDS: &[FieldDef] = &[
    color("color_r", "Color R", default_spot_color_r)
        .with_control(InspectorFieldControl::Number(UNIT_INTERVAL)),
    color("color_g", "Color G", default_spot_color_g)
        .with_control(InspectorFieldControl::Number(UNIT_INTERVAL)),
    color("color_b", "Color B", default_spot_color_b)
        .with_control(InspectorFieldControl::Number(UNIT_INTERVAL)),
    FieldDef::new(
        "intensity",
        "Intensity",
        "Local-light radiant intensity multiplier before distance and cone attenuation.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_spot_intensity),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "range",
        "Range",
        "Maximum world-space influence distance.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_spot_range),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
    FieldDef::new(
        "inner_angle_degrees",
        "Inner Angle",
        "Full-intensity spot-cone half-angle in degrees.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_spot_inner_angle),
    )
    .with_control(InspectorFieldControl::Number(NumericRange::inclusive(
        0.0, 89.0,
    ))),
    FieldDef::new(
        "outer_angle_degrees",
        "Outer Angle",
        "Zero-intensity spot-cone half-angle in degrees; must exceed Inner Angle.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_spot_outer_angle),
    )
    .with_control(InspectorFieldControl::Number(NumericRange::inclusive(
        0.0, 89.0,
    ))),
];

const AMBIENT_LIGHT_FIELDS: &[FieldDef] = &[
    color("color_r", "Color R", default_ambient_color_r),
    color("color_g", "Color G", default_ambient_color_g),
    color("color_b", "Color B", default_ambient_color_b),
    FieldDef::new(
        "intensity",
        "Intensity",
        "Ambient light intensity multiplier.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_ambient_intensity),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
];

const SHADOW_SETTINGS_FIELDS: &[FieldDef] = &[
    FieldDef::new(
        "enabled",
        "Enabled",
        "Render directional-light shadows for this scene.",
        FieldKind::Bool,
        FieldDefaultSpec::Computed(default_shadow_enabled),
    ),
    FieldDef::new(
        "cascade_near_split",
        "Near Cascade Split",
        "Normalized end distance of the first shadow cascade.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_cascade_near_split),
    )
    .with_control(InspectorFieldControl::Number(UNIT_INTERVAL)),
    FieldDef::new(
        "cascade_far_split",
        "Far Cascade Split",
        "Normalized end distance of the second shadow cascade.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_cascade_far_split),
    )
    .with_control(InspectorFieldControl::Number(UNIT_INTERVAL)),
    FieldDef::new(
        "depth_bias",
        "Depth Bias",
        "Constant bias used to reduce shadow acne.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_shadow_depth_bias),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "normal_bias",
        "Normal Bias",
        "Surface-normal bias used to reduce self-shadowing artifacts.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_shadow_normal_bias),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
];

const DIFFUSE_IBL_ENABLED: InspectorFieldCondition = InspectorFieldCondition::Bool {
    field: "diffuse_ibl_enabled",
    equals: true,
};

const ENVIRONMENT_LIGHTING_FIELDS: &[FieldDef] = &[
    FieldDef::new(
        "diffuse_ibl_enabled",
        "Diffuse Environment Enabled",
        "Blend the environment color into the ambient-light contribution.",
        FieldKind::Bool,
        FieldDefaultSpec::Computed(default_diffuse_ibl_enabled),
    ),
    color("color_r", "Color R", default_environment_color_r)
        .with_control(InspectorFieldControl::Number(UNIT_INTERVAL))
        .when(DIFFUSE_IBL_ENABLED),
    color("color_g", "Color G", default_environment_color_g)
        .with_control(InspectorFieldControl::Number(UNIT_INTERVAL))
        .when(DIFFUSE_IBL_ENABLED),
    color("color_b", "Color B", default_environment_color_b)
        .with_control(InspectorFieldControl::Number(UNIT_INTERVAL))
        .when(DIFFUSE_IBL_ENABLED),
    FieldDef::new(
        "intensity",
        "Intensity",
        "Environment-lighting intensity multiplier.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_environment_intensity),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
];

const BLOOM_ENABLED: InspectorFieldCondition = InspectorFieldCondition::Bool {
    field: "bloom_enabled",
    equals: true,
};

const POST_PROCESS_FIELDS: &[FieldDef] = &[
    FieldDef::new(
        "enabled",
        "Enabled",
        "Run the HDR post-processing pass for this scene.",
        FieldKind::Bool,
        FieldDefaultSpec::Computed(default_post_process_enabled),
    ),
    FieldDef::new(
        "exposure",
        "Exposure",
        "Multiplier applied before tone mapping.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_exposure),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "tone_map",
        "Tone Map",
        "Tone-mapping operator used for HDR output.",
        FieldKind::Str,
        FieldDefaultSpec::Computed(default_tone_map),
    )
    .with_control(InspectorFieldControl::Enum(&["aces_fitted", "reinhard"])),
    FieldDef::new(
        "bloom_enabled",
        "Bloom Enabled",
        "Enable the bloom contribution in the post-process pass.",
        FieldKind::Bool,
        FieldDefaultSpec::Computed(default_bloom_enabled),
    ),
    FieldDef::new(
        "bloom_threshold",
        "Bloom Threshold",
        "Luminance threshold above which pixels contribute to bloom.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_bloom_threshold),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE))
    .when(BLOOM_ENABLED),
    FieldDef::new(
        "bloom_intensity",
        "Bloom Intensity",
        "Strength of the bloom contribution.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_bloom_intensity),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE))
    .when(BLOOM_ENABLED),
    FieldDef::new(
        "bloom_radius",
        "Bloom Radius",
        "Approximate bloom blur radius in pixels.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_bloom_radius),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE))
    .when(BLOOM_ENABLED),
    FieldDef::new(
        "color_grading_enabled",
        "Color Grading Enabled",
        "Apply tint, saturation, contrast, and gamma after tone mapping.",
        FieldKind::Bool,
        FieldDefaultSpec::Computed(default_color_grading_enabled),
    ),
    color("grading_tint_r", "Grading Tint R", default_grading_tint_r),
    color("grading_tint_g", "Grading Tint G", default_grading_tint_g),
    color("grading_tint_b", "Grading Tint B", default_grading_tint_b),
    FieldDef::new(
        "grading_saturation",
        "Grading Saturation",
        "Saturation multiplier.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_grading_saturation),
    ),
    FieldDef::new(
        "grading_contrast",
        "Grading Contrast",
        "Contrast multiplier around middle gray.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_grading_contrast),
    ),
    FieldDef::new(
        "grading_gamma",
        "Grading Gamma",
        "Final output gamma.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_grading_gamma),
    ),
];

const PLAYER_CONTROLLER_FIELDS: &[FieldDef] = &[
    FieldDef::new(
        "move_speed",
        "Move Speed",
        "World units moved per second.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_move_speed),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "move_plane",
        "Move Plane",
        "Movement plane: xz or xy.",
        FieldKind::Str,
        FieldDefaultSpec::Computed(default_move_plane),
    )
    .with_control(InspectorFieldControl::Enum(&["xz", "xy"])),
    FieldDef::new(
        "acceleration",
        "Acceleration",
        "Velocity gained per second while movement is requested.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_acceleration),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "deceleration",
        "Deceleration",
        "Velocity removed per second after movement is released.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_deceleration),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "sprint_multiplier",
        "Sprint Multiplier",
        "Move-speed multiplier while the sprint action is held.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_sprint_multiplier),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "camera_relative",
        "Camera Relative",
        "Interpret XZ movement relative to the runtime camera.",
        FieldKind::Bool,
        FieldDefaultSpec::Computed(default_camera_relative),
    ),
    FieldDef::new(
        "face_movement",
        "Face Movement",
        "Turn local forward toward requested XZ movement.",
        FieldKind::Bool,
        FieldDefaultSpec::Computed(default_face_movement),
    ),
];

const ORBIT_CAMERA_FIELDS: &[FieldDef] = &[
    FieldDef::new(
        "target_x",
        "Target X",
        "Orbit center X.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_orbit_target_x),
    ),
    FieldDef::new(
        "target_y",
        "Target Y",
        "Orbit center Y.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_orbit_target_y),
    ),
    FieldDef::new(
        "target_z",
        "Target Z",
        "Orbit center Z.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_orbit_target_z),
    ),
    FieldDef::new(
        "distance",
        "Distance",
        "Distance from target to camera eye.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_orbit_distance),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
    FieldDef::new(
        "yaw",
        "Yaw",
        "Horizontal angle in radians.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_orbit_yaw),
    ),
    FieldDef::new(
        "pitch",
        "Pitch",
        "Vertical elevation angle in radians.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_orbit_pitch),
    ),
    FieldDef::new(
        "orbit_speed",
        "Orbit Speed",
        "Radians per pixel of mouse drag.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_orbit_speed),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "zoom_speed",
        "Zoom Speed",
        "Distance change per scroll unit.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_orbit_zoom_speed),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
];

const FOLLOW_CAMERA_FIELDS: &[FieldDef] = &[
    entity_ref("target", "Target", "The entity this camera follows."),
    float("offset_x", "Offset X", "Camera offset from target, X.", 0.0),
    float("offset_y", "Offset Y", "Camera offset from target, Y.", 2.0),
    float("offset_z", "Offset Z", "Camera offset from target, Z.", 3.0),
    number(
        "spring_strength",
        "Spring Strength",
        "Follow lag: 0=instant, 0.9999=never moves.",
        0.5,
        UNIT_INTERVAL,
    ),
];

const PARTICLE_EMITTER_FIELDS: &[FieldDef] = &[
    asset_ref(
        "mesh",
        "Mesh",
        "The mesh drawn for every particle.",
        AssetKind::Mesh,
        FieldDefaultSpec::Computed(builtin_quad),
    ),
    asset_ref(
        "material",
        "Material",
        "The material drawn on every particle.",
        AssetKind::Material,
        FieldDefaultSpec::Computed(builtin_white_material),
    ),
    FieldDef::new(
        "spawn_rate",
        "Spawn Rate",
        "Particles spawned per second.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_spawn_rate),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "lifetime_min",
        "Lifetime Min",
        "Minimum particle lifetime in seconds.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_lifetime_min),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
    FieldDef::new(
        "lifetime_max",
        "Lifetime Max",
        "Maximum particle lifetime in seconds.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_lifetime_max),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
    FieldDef::new(
        "initial_speed_min",
        "Initial Speed Min",
        "Minimum initial particle speed.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_initial_speed_min),
    ),
    FieldDef::new(
        "initial_speed_max",
        "Initial Speed Max",
        "Maximum initial particle speed.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_initial_speed_max),
    ),
    FieldDef::new(
        "direction_x",
        "Direction X",
        "X component of the base emission direction.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_emitter_direction_x),
    ),
    FieldDef::new(
        "direction_y",
        "Direction Y",
        "Y component of the base emission direction.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_emitter_direction_y),
    ),
    FieldDef::new(
        "direction_z",
        "Direction Z",
        "Z component of the base emission direction.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_emitter_direction_z),
    ),
    FieldDef::new(
        "spread",
        "Spread",
        "Emission cone half-angle in radians.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_spread),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "gravity_x",
        "Gravity X",
        "X component of the world-space acceleration applied to particles.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_emitter_gravity_x),
    ),
    FieldDef::new(
        "gravity_y",
        "Gravity Y",
        "Y component of the world-space acceleration applied to particles.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_emitter_gravity_y),
    ),
    FieldDef::new(
        "gravity_z",
        "Gravity Z",
        "Z component of the world-space acceleration applied to particles.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_emitter_gravity_z),
    ),
    color("start_color_r", "Start Color R", default_start_color_r),
    color("start_color_g", "Start Color G", default_start_color_g),
    color("start_color_b", "Start Color B", default_start_color_b),
    color("start_color_a", "Start Color A", default_start_color_a),
    color("end_color_r", "End Color R", default_end_color_r),
    color("end_color_g", "End Color G", default_end_color_g),
    color("end_color_b", "End Color B", default_end_color_b),
    color("end_color_a", "End Color A", default_end_color_a),
    FieldDef::new(
        "start_size",
        "Start Size",
        "Uniform particle scale at spawn.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_start_size),
    ),
    FieldDef::new(
        "end_size",
        "End Size",
        "Uniform particle scale at the end of a particle's life.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_end_size),
    ),
    FieldDef::new(
        "max_particles",
        "Max Particles",
        "Hard cap on the live particle pool.",
        FieldKind::I64,
        FieldDefaultSpec::Computed(default_max_particles),
    )
    .with_control(InspectorFieldControl::Number(NumericRange::inclusive(
        0.0,
        crate::render_limits::MAX_PARTICLES_PER_EMITTER as f64,
    ))),
    FieldDef::new(
        "seed",
        "Seed",
        "Seed for the emitter's deterministic random stream.",
        FieldKind::I64,
        FieldDefaultSpec::Computed(default_emitter_seed),
    )
    .with_control(InspectorFieldControl::Number(U32_RANGE)),
];

const UI_DOCUMENT_FIELDS: &[FieldDef] = &[];

const COLLIDER_SHAPE_AABB: InspectorFieldCondition = InspectorFieldCondition::String {
    field: "shape",
    equals: "aabb",
};

const COLLIDER_SHAPE_CAPSULE: InspectorFieldCondition = InspectorFieldCondition::String {
    field: "shape",
    equals: "capsule_y",
};

const COLLIDER_SHAPE_ROUND: InspectorFieldCondition = InspectorFieldCondition::StringAny {
    field: "shape",
    values: &["sphere", "capsule_y"],
};

const COLLIDER_FIELDS: &[FieldDef] = &[
    enumeration(
        "shape",
        "Shape",
        "Collider shape: \"aabb\", \"sphere\", or \"capsule_y\".",
        "aabb",
        &["aabb", "sphere", "capsule_y"],
    ),
    number(
        "half_extent_x",
        "Half Extent X",
        "Half-width along X, used by the aabb shape.",
        0.5,
        POSITIVE,
    )
    .when(COLLIDER_SHAPE_AABB),
    number(
        "half_extent_y",
        "Half Extent Y",
        "Half-height along Y, used by the aabb shape.",
        0.5,
        POSITIVE,
    )
    .when(COLLIDER_SHAPE_AABB),
    number(
        "half_extent_z",
        "Half Extent Z",
        "Half-depth along Z, used by the aabb shape.",
        0.5,
        POSITIVE,
    )
    .when(COLLIDER_SHAPE_AABB),
    number(
        "radius",
        "Radius",
        "Radius, used by the sphere and capsule_y shapes.",
        0.5,
        POSITIVE,
    )
    .when(COLLIDER_SHAPE_ROUND),
    number(
        "half_height",
        "Half Height",
        "Half the length of the core segment, used by the capsule_y shape.",
        0.5,
        POSITIVE,
    )
    .when(COLLIDER_SHAPE_CAPSULE),
    boolean(
        "is_trigger",
        "Is Trigger",
        "When true, overlaps produce collision events without push-out.",
        false,
    ),
    FieldDef::new(
        "membership",
        "Membership",
        "Collision layer bitmask this collider belongs to.",
        FieldKind::I64,
        FieldDefaultSpec::I64(1),
    )
    .with_control(InspectorFieldControl::LayerMask),
    FieldDef::new(
        "mask",
        "Mask",
        "Collision layer bitmask this collider tests against.",
        FieldKind::I64,
        FieldDefaultSpec::I64(u32::MAX as i64),
    )
    .with_control(InspectorFieldControl::LayerMask),
];

const PHYSICS_BODY_FIELDS: &[FieldDef] = &[enumeration(
    "kind",
    "Kind",
    "Physics body kind: \"static\", \"kinematic\", or \"dynamic\".",
    "static",
    &["static", "kinematic", "dynamic"],
)];

const CHARACTER_CONTROLLER_FIELDS: &[FieldDef] = &[
    FieldDef::new(
        "gravity_scale",
        "Gravity Scale",
        "Multiplier applied to the world Gravity resource.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_gravity_scale),
    ),
    FieldDef::new(
        "max_resolve_iterations",
        "Max Resolve Iterations",
        "Maximum push-out resolve passes per fixed step (1-16).",
        FieldKind::I64,
        FieldDefaultSpec::Computed(default_max_resolve_iterations),
    )
    .with_control(InspectorFieldControl::Number(NumericRange::inclusive(
        1.0, 16.0,
    ))),
    FieldDef::new(
        "slope_limit_degrees",
        "Slope Limit",
        "Steepest surface treated as walkable ground, in degrees.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_slope_limit_degrees),
    )
    .with_control(InspectorFieldControl::Number(NumericRange::inclusive(
        0.0, 89.0,
    ))),
    FieldDef::new(
        "step_offset",
        "Step Offset",
        "Maximum ledge height climbed while grounded.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_step_offset),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
    FieldDef::new(
        "ground_snap_distance",
        "Ground Snap Distance",
        "Maximum downward snap used to retain ground contact.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_ground_snap_distance),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
    FieldDef::new(
        "skin_width",
        "Skin Width",
        "Small collision margin used by swept movement.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_skin_width),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
];

const DAMAGE_RECEIVER_FIELDS: &[FieldDef] = &[
    FieldDef::new(
        "max_health",
        "Maximum Health",
        "Maximum and initial upper bound for hit points.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_max_health),
    )
    .with_control(InspectorFieldControl::Number(POSITIVE)),
    FieldDef::new(
        "health",
        "Health",
        "Hit points assigned when the entity spawns.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_health),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "invulnerability_seconds",
        "Invulnerability Seconds",
        "Fixed-step immunity duration started after an accepted hit.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_invulnerability_seconds),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "team",
        "Team",
        "Faction ID; attacks from the same team are ignored.",
        FieldKind::I64,
        FieldDefaultSpec::Computed(default_damage_receiver_team),
    )
    .with_control(InspectorFieldControl::Number(NumericRange::inclusive(
        i32::MIN as f64,
        i32::MAX as f64,
    ))),
];

const LOCK_ON_TARGET_FIELDS: &[FieldDef] = &[FieldDef::new(
    "team",
    "Team",
    "Team identifier used by lock-on team filtering.",
    FieldKind::I64,
    FieldDefaultSpec::Computed(default_lock_on_team),
)
.with_control(InspectorFieldControl::Number(U32_RANGE))];

const LOCK_ON_CAMERA_FIELDS: &[FieldDef] = &[
    entity_ref(
        "source",
        "Source",
        "The entity this camera orbits (typically the player).",
    ),
    number(
        "distance",
        "Distance",
        "Distance from source to camera eye.",
        6.0,
        POSITIVE,
    ),
    float("height", "Height", "Vertical offset above source.", 2.5),
    number(
        "spring_strength",
        "Spring Strength",
        "Follow lag: 0=instant, 1=never moves.",
        0.85,
        UNIT_INTERVAL,
    ),
    number(
        "max_target_distance",
        "Max Target Distance",
        "Targets farther than this are never selected.",
        20.0,
        POSITIVE,
    ),
    boolean(
        "require_line_of_sight",
        "Require Line Of Sight",
        "When true, occluded targets are never selected.",
        true,
    ),
    integer(
        "team_filter",
        "Team Filter",
        "Team id accepted by target selection; -1 accepts every team.",
        -1,
        NumericRange::inclusive(-1.0, u32::MAX as f64),
    ),
];

const HAS_NAV_TARGET: InspectorFieldCondition = InspectorFieldCondition::Bool {
    field: "has_target",
    equals: true,
};

const NAV_MESH_AGENT_FIELDS: &[FieldDef] = &[
    FieldDef::new(
        "speed",
        "Speed",
        "Maximum movement speed in world units per second.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_agent_speed),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "stopping_distance",
        "Stopping Distance",
        "Distance from the destination at which movement stops.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_stopping_distance),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "repath_interval",
        "Repath Interval",
        "Seconds between route refreshes for a live target.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_repath_interval),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "avoidance_radius",
        "Avoidance Radius",
        "Personal-space radius used by local agent separation.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_avoidance_radius),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    boolean(
        "has_target",
        "Has Initial Target",
        "Whether the authored target coordinates are active on spawn.",
        false,
    ),
    float("target_x", "Target X", "Initial destination X.", 0.0).when(HAS_NAV_TARGET),
    float("target_y", "Target Y", "Initial destination Y.", 0.0).when(HAS_NAV_TARGET),
    float("target_z", "Target Z", "Initial destination Z.", 0.0).when(HAS_NAV_TARGET),
];

const NAV_MESH_SURFACE_FIELDS: &[FieldDef] = &[asset_ref(
    "source",
    "Baked NavMesh",
    "Registered .navmesh.json artifact produced by the editor bake workflow.",
    AssetKind::NavMesh,
    FieldDefaultSpec::Unassigned,
)];

const RUNTIME_METADATA_FIELDS: &[FieldDef] = &[
    text(
        "name",
        "Runtime Name",
        "Optional gameplay name; blank inherits the authoring entity name.",
        "",
    ),
    list(
        "tags",
        "Tags",
        "String tags used by gameplay queries and filtering.",
        &STRING_KIND,
    ),
    text(
        "team",
        "Team",
        "Team or faction identifier used by combat and targeting.",
        "neutral",
    ),
];

const ANIMATION_CONTROLLER_FIELDS: &[FieldDef] = &[
    boolean(
        "enabled",
        "Enabled",
        "Whether this controller creates playback state for its target rig.",
        true,
    ),
    asset_ref(
        "animation_set",
        "Animation Set",
        "Author-owned motion-slot bindings. Each binding may select an Animation Clip sub-asset from a different imported model source; leave both Animation Set and Animation Graph unassigned for a rest-pose-only rig.",
        AssetKind::AnimationSet,
        FieldDefaultSpec::Unassigned,
    )
    .optional(),
    asset_ref(
        "graph",
        "Animation Graph",
        "Reusable anim.graph controller whose stable motion slots are implemented by Animation Set.",
        AssetKind::AnimationGraph,
        FieldDefaultSpec::Unassigned,
    )
    .optional(),
    boolean(
        "looping",
        "Legacy Looping Fallback",
        "Used only by legacy graph States that do not yet declare a Playback Mode.",
        true,
    ),
    number(
        "playback_speed",
        "Playback Speed",
        "Fixed-step playback multiplier; zero holds the current pose.",
        1.0,
        NON_NEGATIVE,
    ),
    text(
        "completion_event",
        "Completion Event",
        "Event emitted once when a non-looping clip finishes; blank disables it.",
        "animation.completed",
    ),
    enumeration(
        "root_motion_mode",
        "Root Motion",
        "Disabled, extracted for inspection, or collision-resolved by the character motor.",
        "disabled",
        &["disabled", "extracted_only", "applied_to_motor"],
    ),
    number(
        "fade_duration",
        "Fade Duration",
        "Default crossfade duration used by graph state transitions.",
        0.2,
        NON_NEGATIVE,
    ),
    map(
        "parameters",
        "Parameter Defaults",
        "Boolean graph conditions applied before the first runtime tick.",
    )
    .with_control(InspectorFieldControl::StringBoolMap),
];

const BEHAVIOR_TREE_RUNNER_FIELDS: &[FieldDef] = &[
    asset_ref(
        "graph",
        "Graph",
        "Registered graph document whose kind is behavior_tree.graph.",
        AssetKind::BehaviorTree,
        FieldDefaultSpec::Unassigned,
    ),
    map(
        "blackboard",
        "Blackboard Defaults",
        "Initial named values copied into this runtime runner.",
    ),
    boolean(
        "enabled",
        "Enabled",
        "Whether the shared Behavior Tree system ticks this runner.",
        true,
    ),
];

const AUDIO_EMITTER_FIELDS: &[FieldDef] = &[
    asset_ref(
        "clip",
        "Clip",
        "Registered WAV or OGG sound asset.",
        AssetKind::Audio,
        FieldDefaultSpec::Unassigned,
    ),
    number(
        "volume",
        "Volume",
        "Emitter gain from 0.0 to 1.0.",
        1.0,
        UNIT_INTERVAL,
    ),
    number(
        "spatial_blend",
        "Spatial Blend",
        "Blend from 2D (0.0) to positional (1.0); applied by the ER-9 mixer.",
        1.0,
        UNIT_INTERVAL,
    ),
    number(
        "min_distance",
        "Min Distance",
        "Distance where positional attenuation begins.",
        1.0,
        POSITIVE,
    ),
    number(
        "max_distance",
        "Max Distance",
        "Distance where positional attenuation reaches its floor.",
        20.0,
        POSITIVE,
    ),
    boolean(
        "autoplay",
        "Autoplay",
        "Submit one sound-effect request on the first runtime update.",
        false,
    ),
];

const AUDIO_LISTENER_FIELDS: &[FieldDef] = &[boolean(
    "enabled",
    "Enabled",
    "Whether this listener participates in positional mixing.",
    true,
)];

const MUSIC_CONTROLLER_FIELDS: &[FieldDef] = &[
    asset_ref(
        "clip",
        "Clip",
        "Registered WAV or OGG music asset.",
        AssetKind::Audio,
        FieldDefaultSpec::Unassigned,
    ),
    number(
        "volume",
        "Volume",
        "Background-music bus gain from 0.0 to 1.0.",
        1.0,
        UNIT_INTERVAL,
    ),
    number(
        "fade_in_seconds",
        "Fade In",
        "Fade-in or crossfade duration in seconds.",
        0.0,
        NON_NEGATIVE,
    ),
    boolean(
        "autoplay",
        "Autoplay",
        "Start this music on the first runtime update.",
        false,
    ),
];

const FOOT_IK_FIELDS: &[FieldDef] = &[
    FieldDef::new(
        "max_correction",
        "Max Correction",
        "Maximum vertical adjustment in meters; a farther ground hit is treated as no ground.",
        FieldKind::F64,
        FieldDefaultSpec::Computed(default_max_correction),
    )
    .with_control(InspectorFieldControl::Number(NON_NEGATIVE)),
    FieldDef::new(
        "enabled",
        "Enabled",
        "Whether foot IK correction runs for this entity at all.",
        FieldKind::Bool,
        FieldDefaultSpec::Computed(default_foot_ik_enabled),
    ),
];

/// Defines the one-line default providers referenced by the field tables.
///
/// Every scalar default is read from the runtime type's `Default`, so a
/// schema default can never drift from the value the runtime actually uses.
macro_rules! defaults {
    ($($(#[$attr:meta])* $name:ident => $body:expr;)*) => {
        $(
            $(#[$attr])*
            fn $name() -> Value { $body }
        )*
    };
}

fn builtin_asset_value(id: &str) -> Value {
    Value::AssetRef(builtin_asset_id(id))
}

/// Reads the emitter's scalar defaults from `ParticleEmitter::new`.
///
/// The mesh handle is a throwaway: the schema's own `mesh` field defaults to
/// the built-in quad asset, which is unrelated to this placeholder.
fn emitter_defaults() -> ParticleEmitter {
    let mut placeholder_meshes = Assets::<Mesh>::default();
    let placeholder_mesh = placeholder_meshes.add(Mesh::triangle());
    ParticleEmitter::new(placeholder_mesh)
}

fn default_lod_levels() -> Value {
    let mut level = BTreeMap::new();
    level.insert("distance".to_owned(), Value::F64(25.0));
    level.insert(
        "mesh".to_owned(),
        Value::AssetRef(builtin_asset_id(BUILTIN_TRIANGLE_ASSET_ID)),
    );
    Value::Array(vec![Value::Object(level)])
}

defaults! {
    builtin_triangle => builtin_asset_value(BUILTIN_TRIANGLE_ASSET_ID);
    builtin_quad => builtin_asset_value(BUILTIN_QUAD_ASSET_ID);
    builtin_white_material => builtin_asset_value(BUILTIN_WHITE_MATERIAL_ASSET_ID);
    builtin_ui_document => builtin_asset_value(BUILTIN_UI_DOCUMENT_ASSET_ID);

    default_camera_enabled => Value::Bool(Camera3D::default().enabled);
    default_camera_priority => Value::I64(i64::from(Camera3D::default().priority));
    default_camera_fov => Value::F64(Camera3D::default().fov_y_radians.to_degrees() as f64);
    default_camera_near => Value::F64(Camera3D::default().near as f64);
    default_camera_far => Value::F64(Camera3D::default().far as f64);

    default_sun_direction_x => Value::F64(DirectionalLight::default().direction.x as f64);
    default_sun_direction_y => Value::F64(DirectionalLight::default().direction.y as f64);
    default_sun_direction_z => Value::F64(DirectionalLight::default().direction.z as f64);
    default_sun_color_r => Value::F64(DirectionalLight::default().color.x as f64);
    default_sun_color_g => Value::F64(DirectionalLight::default().color.y as f64);
    default_sun_color_b => Value::F64(DirectionalLight::default().color.z as f64);
    default_sun_intensity => Value::F64(DirectionalLight::default().intensity as f64);

    default_point_color_r => Value::F64(PointLight::default().color.x as f64);
    default_point_color_g => Value::F64(PointLight::default().color.y as f64);
    default_point_color_b => Value::F64(PointLight::default().color.z as f64);
    default_point_intensity => Value::F64(PointLight::default().intensity as f64);
    default_point_range => Value::F64(PointLight::default().range as f64);

    default_spot_color_r => Value::F64(SpotLight::default().color.x as f64);
    default_spot_color_g => Value::F64(SpotLight::default().color.y as f64);
    default_spot_color_b => Value::F64(SpotLight::default().color.z as f64);
    default_spot_intensity => Value::F64(SpotLight::default().intensity as f64);
    default_spot_range => Value::F64(SpotLight::default().range as f64);
    default_spot_inner_angle => Value::F64(SpotLight::default().inner_angle_radians.to_degrees() as f64);
    default_spot_outer_angle => Value::F64(SpotLight::default().outer_angle_radians.to_degrees() as f64);

    default_ambient_color_r => Value::F64(AmbientLight::default().color.x as f64);
    default_ambient_color_g => Value::F64(AmbientLight::default().color.y as f64);
    default_ambient_color_b => Value::F64(AmbientLight::default().color.z as f64);
    default_ambient_intensity => Value::F64(AmbientLight::default().intensity as f64);

    default_shadow_enabled => Value::Bool(ShadowSettings::default().enabled);
    default_cascade_near_split => Value::F64(ShadowSettings::default().cascade_splits[0] as f64);
    default_cascade_far_split => Value::F64(ShadowSettings::default().cascade_splits[1] as f64);
    default_shadow_depth_bias => Value::F64(ShadowSettings::default().depth_bias as f64);
    default_shadow_normal_bias => Value::F64(ShadowSettings::default().normal_bias as f64);

    default_diffuse_ibl_enabled => Value::Bool(EnvironmentLighting::default().diffuse_ibl_enabled);
    default_environment_color_r => Value::F64(EnvironmentLighting::default().diffuse_color.x as f64);
    default_environment_color_g => Value::F64(EnvironmentLighting::default().diffuse_color.y as f64);
    default_environment_color_b => Value::F64(EnvironmentLighting::default().diffuse_color.z as f64);
    default_environment_intensity => Value::F64(EnvironmentLighting::default().intensity as f64);

    default_post_process_enabled => Value::Bool(PostProcessSettings::default().enabled);
    default_exposure => Value::F64(PostProcessSettings::default().exposure as f64);
    default_tone_map => Value::String(
        match PostProcessSettings::default().tone_map {
            ToneMapOperator::AcesFitted => "aces_fitted",
            ToneMapOperator::Reinhard => "reinhard",
        }
        .to_owned(),
    );
    default_bloom_enabled => Value::Bool(PostProcessSettings::default().bloom.enabled);
    default_bloom_threshold => Value::F64(PostProcessSettings::default().bloom.threshold as f64);
    default_bloom_intensity => Value::F64(PostProcessSettings::default().bloom.intensity as f64);
    default_bloom_radius => Value::F64(PostProcessSettings::default().bloom.radius as f64);
    default_color_grading_enabled => Value::Bool(PostProcessSettings::default().color_grading.enabled);
    default_grading_tint_r => Value::F64(PostProcessSettings::default().color_grading.tint[0] as f64);
    default_grading_tint_g => Value::F64(PostProcessSettings::default().color_grading.tint[1] as f64);
    default_grading_tint_b => Value::F64(PostProcessSettings::default().color_grading.tint[2] as f64);
    default_grading_saturation => Value::F64(PostProcessSettings::default().color_grading.saturation as f64);
    default_grading_contrast => Value::F64(PostProcessSettings::default().color_grading.contrast as f64);
    default_grading_gamma => Value::F64(PostProcessSettings::default().color_grading.gamma as f64);

    default_move_speed => Value::F64(PlayerController::default().move_speed as f64);
    default_move_plane => Value::String(
        match PlayerController::default().move_plane {
            crate::player::MovePlane::Xz => "xz",
            crate::player::MovePlane::Xy => "xy",
        }
        .to_owned(),
    );
    default_acceleration => Value::F64(PlayerController::default().acceleration as f64);
    default_deceleration => Value::F64(PlayerController::default().deceleration as f64);
    default_sprint_multiplier => Value::F64(PlayerController::default().sprint_multiplier as f64);
    default_camera_relative => Value::Bool(PlayerController::default().camera_relative);
    default_face_movement => Value::Bool(PlayerController::default().face_movement);

    default_orbit_target_x => Value::F64(OrbitCamera::default().target.x as f64);
    default_orbit_target_y => Value::F64(OrbitCamera::default().target.y as f64);
    default_orbit_target_z => Value::F64(OrbitCamera::default().target.z as f64);
    default_orbit_distance => Value::F64(OrbitCamera::default().distance as f64);
    default_orbit_yaw => Value::F64(OrbitCamera::default().yaw as f64);
    default_orbit_pitch => Value::F64(OrbitCamera::default().pitch as f64);
    default_orbit_speed => Value::F64(OrbitCamera::default().orbit_speed as f64);
    default_orbit_zoom_speed => Value::F64(OrbitCamera::default().zoom_speed as f64);

    default_spawn_rate => Value::F64(emitter_defaults().spawn_rate as f64);
    default_lifetime_min => Value::F64(emitter_defaults().lifetime.0 as f64);
    default_lifetime_max => Value::F64(emitter_defaults().lifetime.1 as f64);
    default_initial_speed_min => Value::F64(emitter_defaults().initial_speed.0 as f64);
    default_initial_speed_max => Value::F64(emitter_defaults().initial_speed.1 as f64);
    default_emitter_direction_x => Value::F64(emitter_defaults().direction.x as f64);
    default_emitter_direction_y => Value::F64(emitter_defaults().direction.y as f64);
    default_emitter_direction_z => Value::F64(emitter_defaults().direction.z as f64);
    default_spread => Value::F64(emitter_defaults().spread as f64);
    default_emitter_gravity_x => Value::F64(emitter_defaults().gravity.x as f64);
    default_emitter_gravity_y => Value::F64(emitter_defaults().gravity.y as f64);
    default_emitter_gravity_z => Value::F64(emitter_defaults().gravity.z as f64);
    default_start_color_r => Value::F64(emitter_defaults().start_color[0] as f64);
    default_start_color_g => Value::F64(emitter_defaults().start_color[1] as f64);
    default_start_color_b => Value::F64(emitter_defaults().start_color[2] as f64);
    default_start_color_a => Value::F64(emitter_defaults().start_color[3] as f64);
    default_end_color_r => Value::F64(emitter_defaults().end_color[0] as f64);
    default_end_color_g => Value::F64(emitter_defaults().end_color[1] as f64);
    default_end_color_b => Value::F64(emitter_defaults().end_color[2] as f64);
    default_end_color_a => Value::F64(emitter_defaults().end_color[3] as f64);
    default_start_size => Value::F64(emitter_defaults().start_size as f64);
    default_end_size => Value::F64(emitter_defaults().end_size as f64);
    default_max_particles => Value::I64(emitter_defaults().max_particles as i64);
    default_emitter_seed => Value::I64(emitter_defaults().seed as i64);

    default_gravity_scale => Value::F64(KinematicCharacterController::default().gravity_scale as f64);
    default_max_resolve_iterations =>
        Value::I64(KinematicCharacterController::default().max_resolve_iterations as i64);
    default_slope_limit_degrees =>
        Value::F64(KinematicCharacterController::default().slope_limit_degrees as f64);
    default_step_offset => Value::F64(KinematicCharacterController::default().step_offset as f64);
    default_ground_snap_distance =>
        Value::F64(KinematicCharacterController::default().ground_snap_distance as f64);
    default_skin_width => Value::F64(KinematicCharacterController::default().skin_width as f64);

    default_max_health => Value::F64(DamageReceiver::default().max_health as f64);
    default_health => Value::F64(DamageReceiver::default().health as f64);
    default_invulnerability_seconds =>
        Value::F64(DamageReceiver::default().invulnerability_seconds as f64);
    default_damage_receiver_team => Value::I64(i64::from(DamageReceiver::default().team));

    default_lock_on_team => Value::I64(i64::from(LockOnTarget::default().team));

    default_agent_speed => Value::F64(NavMeshAgent::default().speed as f64);
    default_stopping_distance => Value::F64(NavMeshAgent::default().stopping_distance as f64);
    default_repath_interval => Value::F64(NavMeshAgent::default().repath_interval as f64);
    default_avoidance_radius => Value::F64(NavMeshAgent::default().avoidance_radius as f64);

    default_max_correction => Value::F64(f64::from(FootIk::default().max_correction));
    default_foot_ik_enabled => Value::Bool(FootIk::default().enabled);
}

fn transform_schema() -> engine_authoring::schema::ComponentSchema {
    authoring_builtin_schema(TRANSFORM_COMPONENT)
}

fn player_marker_schema() -> engine_authoring::schema::ComponentSchema {
    authoring_builtin_schema(PLAYER_MARKER_COMPONENT)
}

/// Every built-in authorable component, in registration order.
///
/// Registration order is part of the authoring contract: component pickers
/// and golden scene output both follow it, so new entries go at the end.
pub(super) fn builtin_components() -> Vec<BuiltinComponent> {
    vec![
        BuiltinComponent::new(
            TRANSFORM_COMPONENT,
            "Transform",
            "Local position, Euler rotation, and scale for an entity.",
            "Engine",
            2,
            &[],
            spawn_transform_component,
        )
        .schema_from(transform_schema)
        .generic_inspector(),
        BuiltinComponent::new(
            PLAYER_MARKER_COMPONENT,
            "Player Marker",
            "Marks this entity as the player.",
            "Engine",
            1,
            &[],
            spawn_player_marker_component,
        )
        .schema_from(player_marker_schema)
        .generic_inspector(),
        BuiltinComponent::new(
            STATIC_MESH_RENDERER_COMPONENT,
            "Static Mesh Renderer",
            "Draws one static mesh with a base material and optional per-submesh materials.",
            "Rendering",
            1,
            STATIC_MESH_RENDERER_FIELDS,
            spawn_static_mesh_renderer_component,
        ),
        BuiltinComponent::new(
            SKINNED_MESH_RENDERER_COMPONENT,
            "Skinned Mesh Renderer",
            "Draws one imported mesh deformed by a rig with optional material slots.",
            "Rendering",
            2,
            SKINNED_MESH_RENDERER_FIELDS,
            spawn_skinned_mesh_renderer_component,
        ),
        BuiltinComponent::new(
            SKINNED_MODEL_COMPONENT,
            "Skinned Model",
            "Creates one character rig from an imported skeleton.",
            "Rendering",
            1,
            SKINNED_MODEL_FIELDS,
            spawn_skinned_model_component,
        )
        .collapsed_by_default(),
        BuiltinComponent::new(
            BONE_ATTACHMENT_COMPONENT,
            "Bone Attachment",
            "Follows one bone of a Skinned Model's rig; this entity's Transform becomes the offset from that bone.",
            "Rendering",
            1,
            BONE_ATTACHMENT_FIELDS,
            spawn_bone_attachment_component,
        ),
        BuiltinComponent::new(
            LOD_GROUP_COMPONENT,
            "LOD Group",
            "Chooses successively cheaper meshes as camera distance increases.",
            "Rendering",
            1,
            LOD_GROUP_FIELDS,
            spawn_lod_group_component,
        )
        .collapsed_by_default(),
        BuiltinComponent::new(
            CAMERA_COMPONENT,
            "Camera",
            "Perspective camera selected for the Game View by enabled state and priority.",
            "Engine",
            2,
            CAMERA_FIELDS,
            spawn_camera_component,
        ),
        BuiltinComponent::new(
            DIRECTIONAL_LIGHT_COMPONENT,
            "Directional Light",
            "Entity-authored sun-like light mirrored to the render resource.",
            "Engine",
            1,
            DIRECTIONAL_LIGHT_FIELDS,
            spawn_directional_light_component,
        ),
        BuiltinComponent::new(
            AMBIENT_LIGHT_COMPONENT,
            "Ambient Light",
            "Entity-authored ambient light mirrored to the render resource.",
            "Engine",
            1,
            AMBIENT_LIGHT_FIELDS,
            spawn_ambient_light_component,
        ),
        BuiltinComponent::new(
            SHADOW_SETTINGS_COMPONENT,
            "Shadow Settings",
            "Scene-owned directional shadow cascade and bias controls.",
            "Rendering",
            1,
            SHADOW_SETTINGS_FIELDS,
            spawn_shadow_settings_component,
        ),
        BuiltinComponent::new(
            ENVIRONMENT_LIGHTING_COMPONENT,
            "Environment Lighting",
            "Scene-owned diffuse environment-light contribution.",
            "Rendering",
            1,
            ENVIRONMENT_LIGHTING_FIELDS,
            spawn_environment_lighting_component,
        ),
        BuiltinComponent::new(
            POST_PROCESS_COMPONENT,
            "Post Process",
            "Scene-owned HDR exposure, tone mapping, and bloom controls.",
            "Rendering",
            1,
            POST_PROCESS_FIELDS,
            spawn_post_process_component,
        )
        .collapsed_by_default(),
        BuiltinComponent::new(
            PLAYER_CONTROLLER_COMPONENT,
            "Player Controller",
            "Converts configured actions into fixed-step kinematic movement.",
            "Engine",
            1,
            PLAYER_CONTROLLER_FIELDS,
            spawn_player_controller_component,
        ),
        BuiltinComponent::new(
            ORBIT_CAMERA_COMPONENT,
            "Orbit Camera",
            "Mouse-controlled camera that orbits around a fixed world-space target.",
            "Engine",
            1,
            ORBIT_CAMERA_FIELDS,
            spawn_orbit_camera_component,
        ),
        BuiltinComponent::new(
            FOLLOW_CAMERA_COMPONENT,
            "Follow Camera",
            "Smoothly follows a target entity using exponential spring damping.",
            "Engine",
            1,
            FOLLOW_CAMERA_FIELDS,
            spawn_follow_camera_component,
        ),
        BuiltinComponent::new(
            PARTICLE_EMITTER_COMPONENT,
            "Particle Emitter",
            "Emits and simulates a pool of instanced particles from this entity.",
            "Engine",
            2,
            PARTICLE_EMITTER_FIELDS,
            spawn_particle_emitter_component,
        )
        .collapsed_by_default(),
        BuiltinComponent::new(
            UI_DOCUMENT_COMPONENT,
            "UI Document",
            "The declarative UI document drawn for this entity (Phase 54 / ADR 0046).",
            "Engine",
            1,
            UI_DOCUMENT_FIELDS,
            spawn_ui_document_component,
        )
        .asset_value(AssetKind::UiDocument, builtin_ui_document),
        BuiltinComponent::new(
            COLLIDER_COMPONENT,
            "Collider",
            "Collision shape for broad/narrow-phase overlap detection (Phase 57).",
            "Engine",
            1,
            COLLIDER_FIELDS,
            spawn_collider_component,
        ),
        BuiltinComponent::new(
            PHYSICS_BODY_COMPONENT,
            "Physics Body",
            "Push-out participation kind for collision resolution (Phase 57).",
            "Engine",
            1,
            PHYSICS_BODY_FIELDS,
            spawn_physics_body_component,
        ),
        BuiltinComponent::new(
            CHARACTER_CONTROLLER_COMPONENT,
            "Character Controller",
            "Swept, grounded kinematic character controller with character separation.",
            "Engine",
            2,
            CHARACTER_CONTROLLER_FIELDS,
            spawn_character_controller_component,
        ),
        BuiltinComponent::new(
            DAMAGE_RECEIVER_COMPONENT,
            "Damage Receiver",
            "Health, team filtering, and post-hit invulnerability state.",
            "Gameplay",
            1,
            DAMAGE_RECEIVER_FIELDS,
            spawn_damage_receiver_component,
        ),
        BuiltinComponent::new(
            LOCK_ON_TARGET_COMPONENT,
            "Lock-On Target",
            "Marks this entity as a valid lock-on target (Phase 58).",
            "Engine",
            1,
            LOCK_ON_TARGET_FIELDS,
            spawn_lock_on_target_component,
        ),
        BuiltinComponent::new(
            LOCK_ON_CAMERA_COMPONENT,
            "Lock-On Camera",
            "Frames the locked-on target from a source entity, with wall avoidance (Phase 58).",
            "Engine",
            1,
            LOCK_ON_CAMERA_FIELDS,
            spawn_lock_on_camera_component,
        ),
        BuiltinComponent::new(
            NAV_MESH_AGENT_COMPONENT,
            "NavMesh Agent",
            "Moves toward an optional world-space target on the runtime NavMesh.",
            "Navigation",
            2,
            NAV_MESH_AGENT_FIELDS,
            spawn_nav_mesh_agent_component,
        ),
        BuiltinComponent::new(
            NAV_MESH_SURFACE_COMPONENT,
            "NavMesh Surface",
            "Loads one baked navigation artifact for this scene.",
            "Navigation",
            1,
            NAV_MESH_SURFACE_FIELDS,
            spawn_nav_mesh_surface_component,
        ),
        BuiltinComponent::new(
            RUNTIME_METADATA_COMPONENT,
            "Runtime Metadata",
            "Gameplay-facing name, classification tags, and team.",
            "Gameplay",
            1,
            RUNTIME_METADATA_FIELDS,
            spawn_runtime_metadata_component,
        )
        .generic_inspector(),
        BuiltinComponent::new(
            ANIMATION_CONTROLLER_COMPONENT,
            "Animation Controller",
            "Plays a reusable animation graph on the rig owned by this entity's Skinned Model, using an animation set that binds graph motion slots to clips.",
            "Animation",
            4,
            ANIMATION_CONTROLLER_FIELDS,
            spawn_animation_controller_component,
        ),
        BuiltinComponent::new(
            BEHAVIOR_TREE_RUNNER_COMPONENT,
            "Behavior Tree Runner",
            "Ticks a compiled Behavior Tree with authored blackboard defaults.",
            "AI",
            1,
            BEHAVIOR_TREE_RUNNER_FIELDS,
            spawn_behavior_tree_runner_component,
        ),
        BuiltinComponent::new(
            AUDIO_EMITTER_COMPONENT,
            "Audio Emitter",
            "Plays a registered sound from this entity's position.",
            "Audio",
            1,
            AUDIO_EMITTER_FIELDS,
            spawn_audio_emitter_component,
        ),
        BuiltinComponent::new(
            AUDIO_LISTENER_COMPONENT,
            "Audio Listener",
            "Marks this entity as a positional-audio listener.",
            "Audio",
            1,
            AUDIO_LISTENER_FIELDS,
            spawn_audio_listener_component,
        )
        .generic_inspector(),
        BuiltinComponent::new(
            MUSIC_CONTROLLER_COMPONENT,
            "Music Controller",
            "Starts looping background music through the shared audio mixer.",
            "Audio",
            1,
            MUSIC_CONTROLLER_FIELDS,
            spawn_music_controller_component,
        ),
        BuiltinComponent::new(
            FOOT_IK_COMPONENT,
            "Foot IK",
            "Runtime two-bone foot IK correction against detected ground contacts (ADR 0080).",
            "Animation",
            1,
            FOOT_IK_FIELDS,
            spawn_foot_ik_component,
        ),
        BuiltinComponent::new(
            RIGID_BODY_PHYSICS_COMPONENT,
            "Secondary Motion",
            "Simulates the assigned engine-native Secondary Motion Rig for hair, skirts, and accessories.",
            "Physics",
            1,
            RIGID_BODY_PHYSICS_FIELDS,
            spawn_rigid_body_physics_component,
        ),
        BuiltinComponent::new(
            POINT_LIGHT_COMPONENT,
            "Point Light",
            "Finite-range omnidirectional light positioned by this entity's Transform.",
            "Rendering",
            1,
            POINT_LIGHT_FIELDS,
            spawn_point_light_component,
        ),
        BuiltinComponent::new(
            SPOT_LIGHT_COMPONENT,
            "Spot Light",
            "Finite-range cone light positioned by this entity's Transform and aimed along local -Z.",
            "Rendering",
            1,
            SPOT_LIGHT_FIELDS,
            spawn_spot_light_component,
        ),
    ]
}
