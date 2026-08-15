//! Format-independent model intermediate representation (ADR 0078).
//!
//! `ModelDocument` is the boundary between a format parser (today, the
//! glTF/GLB parser in `crate::gltf_import`) and the format-agnostic asset
//! runtime-asset builder. A parser's only job is to turn a
//! source file into a `ModelDocument`; every later stage — sub-asset ID
//! derivation, skeleton identity/dedupe/rebind (ADR 0077), clip
//! stable runtime bone-ID resolution — reads only this type and
//! must never branch on which format produced it.
//!
//! # Normalization contract
//!
//! A `ModelDocument` is always already normalized to engine conventions.
//! Concretely, whoever constructs one guarantees:
//!
//! - **Units**: meters.
//! - **Axes**: right-handed, +Y up (the engine/glTF convention).
//! - **Node transforms**: `IrNode::translation` / `IrNode::rotation` /
//!   `IrNode::scale` are a plain local TRS. No format-specific pivot,
//!   PreRotation, GeometricTransform, or axis-conversion residue may survive
//!   into the IR; a parser that needs to bake one of those away must do so
//!   before filling in the node.
//! - **Keyframe times**: seconds, measured from the start of the clip.
//! - **Rotations**: unit quaternions (both node rest rotations and rotation
//!   keyframe values).
//! - **Interpolation**: every `IrClipChannel` is already linear-sampled.
//!   Non-`LINEAR` source interpolation (glTF `STEP` / `CUBICSPLINE`, or any
//!   other format's equivalent) is resampled or downgraded by the parser
//!   before it reaches the IR, with a diagnostic recorded on
//!   `ModelDocument::diagnostics` (existing rule, ADR 0043).
//! - **Skeleton scope**: `ModelDocument::skeleton_scope` states whether
//!   `ModelDocument::skins` each build their own skeleton or share one
//!   document-wide skeleton (ADR 0097 §4a). This is a general "do these
//!   skins share a rig?" question that any format can answer, not residue
//!   from a particular format's structure, so it belongs in the IR itself
//!   rather than as a builder-side heuristic.
//!
//! Builder tests construct `ModelDocument` values by hand (no format file
//! involved) specifically to keep the builder's logic exercised without a
//! parser in the loop; see the model-builder acceptance tests.
//!
//! # Selector fields and cross-references
//!
//! Every entry that can be dropped by a parser (a skin with too many
//! joints, a mesh whose primitives all failed to decode, a texture with an
//! unsupported pixel format, ...) carries its own `source_index`: the
//! original zero-based selector in the source document. This is not part of
//! the illustrative shape ADR 0078 sketches, but it is required to satisfy
//! that same ADR's compatibility section — sub-asset IDs are derived from
//! the *original* source selector (`imported_sub_asset_id`, unchanged), so a
//! dropped entry earlier in the document must not shift the selector of
//! entries after it. Every cross-reference field (an `IrNode`'s `mesh` /
//! `skin`, an `IrMesh`'s material slots, an `IrMaterial`'s texture
//! slots) likewise always stores the *original* selector of the referenced
//! entry, never a position that could shift when something else is dropped;
//! The runtime-asset builder resolves those selectors against each list's own
//! `source_index` fields.

use engine_authoring::diagnostic::Diagnostic;
use engine_authoring::{
    LinearRgba, MaterialAlphaMode, MaterialCullMode, MaterialOutline, MaterialShadingModel,
    MaterialSphereBlendMode, MaterialSphereCoordinateSource,
};
use glam::{Mat4, Quat, Vec3};

/// Transform property carried by a format-independent animation channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrAnimProperty {
    /// Local translation.
    Translation,
    /// Local rotation.
    Rotation,
    /// Local scale.
    Scale,
}

/// One format-independent animation sample.
#[derive(Debug, Clone, PartialEq)]
pub struct IrKeyframe {
    /// Time in seconds from the start of the source clip.
    pub time: f32,
    /// XYZ for translation/scale or XYZW for rotation.
    pub value: [f32; 4],
}

/// CPU vertex data carried across the parser/builder boundary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IrVertex {
    /// Object-space position.
    pub position: [f32; 3],
    /// Object-space normal.
    pub normal: [f32; 3],
    /// Vertex RGB color.
    pub color: [f32; 3],
    /// Texture coordinate.
    pub uv: [f32; 2],
    /// Per-vertex outline-width multiplier.
    pub outline_scale: f32,
    /// First generic additional UV channel.
    pub additional_uv: [f32; 2],
}

/// Per-vertex skinning data carried by the neutral model IR.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IrSkinningVertexData {
    /// Indices into the source skin's joint array.
    pub joints: [u16; 4],
    /// Normalized blend weights matching [`Self::joints`].
    pub weights: [f32; 4],
}

/// One contiguous material range in [`IrMeshData`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrSubmesh {
    /// First index, or first vertex for unindexed geometry.
    pub start: u32,
    /// Number of indices, or vertices for unindexed geometry.
    pub count: u32,
}

/// Format-independent CPU mesh payload.
///
/// This deliberately mirrors only data that importers can produce. GPU
/// layouts, validation policy, and rendering methods belong to the runtime
/// mesh type and are created by the model builder after this boundary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IrMeshData {
    /// Vertices in source draw order.
    pub vertices: Vec<IrVertex>,
    /// Optional triangle-list indices.
    pub indices: Option<Vec<u32>>,
    /// Optional per-vertex skinning attributes.
    pub skinning: Option<Vec<IrSkinningVertexData>>,
    /// Optional source tangent vectors and handedness.
    pub tangents: Option<Vec<[f32; 4]>>,
    /// Material draw ranges; empty means one full range.
    pub submeshes: Vec<IrSubmesh>,
}

/// Whether a [`ModelDocument`]'s skins each define their own skeleton or
/// share one skeleton spanning an explicit, format-declared node set (ADR
/// 0097 §4a).
///
/// This is a general IR concept, not format-specific residue: "do these
/// skins share a rig?" is a question any format's parser can answer, so it
/// satisfies this module's normalization contract the same way every other
/// field does. glTF/FBX answer [`Self::PerSkin`] (a skin's joint list *is*
/// its skeleton in those formats); PMX answers
/// [`Self::SharedAcrossDocument`] because a single PMX rig is split into
/// several render-part skins (ADR 0097 §4) that must still bind to one
/// shared rig, not one duplicated skeleton per part.
///
/// [`Self::SharedAcrossDocument`] carries its node set explicitly
/// (`skeleton_nodes`) rather than the builder inferring it (for example, by
/// assuming every node without a mesh is a bone): a node being both a joint
/// and a mesh anchor is legitimate in some formats, so inference would be
/// fragile and format-leaky, and — critically — [`ModelDocument::nodes`]
/// commonly contains entries that are not part of the rig at all (PMX's
/// per-split-part mesh anchor nodes, see the PMX importer). Including
/// those in the skeleton would make runtime skeleton identity
/// (ADR 0077 §4) depend on how the document's meshes happened to be
/// partitioned rather than on the rig itself, which would spuriously trip
/// ADR 0077's rebind path whenever the partitioning changed without the rig
/// changing at all. An explicit `skeleton_nodes` list makes that invalid
/// coupling unrepresentable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SkeletonScope {
    /// Each skin defines its own skeleton from its joints and their
    /// ancestors. glTF/FBX behavior, unchanged, and the default.
    #[default]
    PerSkin,
    /// Every skin binds to one shared skeleton spanning exactly
    /// `skeleton_nodes` (and their ancestors), as ADR 0086 §4 permits: the
    /// skeleton may exceed `MAX_JOINTS` so long as no single skin does.
    SharedAcrossDocument {
        /// Indices into [`ModelDocument::nodes`] that form the shared rig.
        ///
        /// Nodes outside this list — mesh anchors, scene-graph scaffolding —
        /// are deliberately excluded so the skeleton's identity (ADR 0077)
        /// depends only on the rig itself and never on how the document's
        /// meshes happen to be partitioned.
        skeleton_nodes: Vec<usize>,
    },
}

/// A fully parsed, format-independent model source (ADR 0078).
///
/// See the module documentation for the normalization contract every field
/// satisfies and for how selector/cross-reference fields behave across
/// dropped entries.
#[derive(Clone, Default)]
pub struct ModelDocument {
    /// Every node in the source's scene graph, indexed by its original
    /// selector; [`IrNode::parent`] indexes into this same list.
    pub nodes: Vec<IrNode>,
    /// Meshes that survived parsing (at least one primitive decoded).
    pub meshes: Vec<IrMesh>,
    /// Skins that survived structural validation (joint count and joint
    /// node references within range).
    pub skins: Vec<IrSkin>,
    /// Whether [`Self::skins`] each define their own skeleton or share one
    /// document-wide skeleton (ADR 0097 §4a). Defaults to
    /// [`SkeletonScope::PerSkin`], which is exactly today's glTF/FBX
    /// behavior, so every existing construction site (including
    /// `ModelDocument::default()`) keeps producing byte-identical results.
    pub skeleton_scope: SkeletonScope,
    /// Animation clips, one per source animation that contributed at least
    /// one channel bound to a skin joint.
    pub clips: Vec<IrClip>,
    /// Materials converted to the engine's material contract.
    pub materials: Vec<IrMaterial>,
    /// Textures that decoded successfully.
    pub textures: Vec<IrTexture>,
    /// A secondary-motion rigid-body rig declared by the source, if any
    /// (ADR 0097 §6).
    ///
    /// `None` for every source that declares no such rig, which is every
    /// glTF and FBX document today. Like [`SkeletonScope`], this is a general
    /// IR concept rather than format residue: "does this model ship a
    /// physics rig for its own secondary motion?" is a question any format
    /// can answer, and the answer is expressed in engine units and axes like
    /// every other field here.
    pub rigid_body_rig: Option<IrRigidBodyRig>,
    /// Non-fatal diagnostics recorded while parsing (missing attributes,
    /// unsupported extensions, renormalized weights, downgraded
    /// interpolation, and similar).
    pub diagnostics: Vec<Diagnostic>,
}

/// One node in a [`ModelDocument`]'s scene graph.
#[derive(Debug, Clone)]
pub struct IrNode {
    /// Human-readable node name from the source document.
    pub name: String,
    /// Index into [`ModelDocument::nodes`] of this node's parent, if any.
    pub parent: Option<usize>,
    /// Local translation (plain TRS; see the module normalization contract).
    pub translation: Vec3,
    /// Local rotation.
    pub rotation: Quat,
    /// Local scale.
    pub scale: Vec3,
    /// Original selector of the mesh this node instantiates, if any.
    ///
    /// Always the *source* selector (matches some [`IrMesh::source_index`]
    /// when that mesh survived parsing; the referenced mesh may be absent
    /// from [`ModelDocument::meshes`] when every one of its primitives
    /// failed to decode, exactly like today's `GltfNodeBinding`).
    pub mesh: Option<usize>,
    /// Original selector of the skin this node binds, if any.
    ///
    /// Always the *source* selector (matches some [`IrSkin::source_index`]
    /// when that skin survived structural validation).
    pub skin: Option<usize>,
}

/// One mesh extracted from a [`ModelDocument`]'s source.
#[derive(Clone)]
pub struct IrMesh {
    /// Original zero-based selector in the source document.
    pub source_index: usize,
    /// Human-readable mesh name.
    pub name: String,
    /// CPU-side geometry, including skinning attributes and submesh ranges.
    pub mesh: IrMeshData,
    /// Material reference per submesh, in submesh order.
    ///
    /// Each entry is the *source* selector of the referenced material
    /// (materials are never dropped by a parser, so this always matches an
    /// [`IrMaterial::source_index`]); `None` when that submesh declares no
    /// material.
    pub submesh_materials: Vec<Option<usize>>,
    /// Named vertex deformations this mesh can blend (ADR 0097 §5).
    ///
    /// Empty for a source that declares none. Not PMX-specific:
    /// The glTF importer detects glTF morph targets today and drops them
    /// with a `gltf.morph_targets_unsupported` diagnostic, and can be wired
    /// through this same field without any further runtime work.
    pub morph_targets: Vec<IrMorphTarget>,
}

/// One named vertex deformation a mesh can blend (ADR 0097 §5).
///
/// Deltas are **sparse**: only the vertices a morph actually moves appear, in
/// ascending `vertex_index` order, using this mesh's own vertex indices (the
/// same indices [`IrMesh::mesh`] uses, after any importer-side splitting).
/// A facial morph typically touches a few hundred of a character's hundred
/// thousand vertices, so a dense per-morph array would cost three orders of
/// magnitude more memory for no gain.
#[derive(Debug, Clone)]
pub struct IrMorphTarget {
    /// Original zero-based selector in the source document.
    ///
    /// Stable across reimports of the same file, and the index the morph's
    /// sub-asset ID derives from, so an author's binding survives a
    /// re-export that keeps morph order.
    pub source_index: usize,
    /// Human-readable morph name, as motion sources address it by name.
    pub name: String,
    /// Per-vertex position deltas in engine units, `(vertex_index, delta)`,
    /// sorted by `vertex_index`.
    ///
    /// Empty for a morph that only changes material parameters.
    pub vertex_deltas: Vec<(u32, Vec3)>,
    /// Per-material parameter overrides this morph applies, if any.
    pub material_offsets: Vec<IrMaterialMorphOffset>,
}

/// How a material morph combines its values with the material's own
/// (ADR 0097 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IrMaterialMorphOperation {
    /// Interpolate from the material's own value toward the morph's, by
    /// weight. MMD calls this "multiply", because its stored values are
    /// factors around 1.0 rather than absolute colors.
    #[default]
    Multiply,
    /// Add the morph's value scaled by weight on top of the material's own.
    Add,
}

/// One material's parameter overrides within an [`IrMorphTarget`].
///
/// Only the fields the engine's material contract actually has are carried:
/// a source's toon, sphere-map, and edge parameters have no representation to
/// override (see [`MaterialShadingModel`]), so they are dropped at parse time
/// rather than stored as data nothing reads.
#[derive(Debug, Clone)]
pub struct IrMaterialMorphOffset {
    /// The *source* selector of the affected material, matching some
    /// [`IrMaterial::source_index`]. A source-wide morph (MMD's material
    /// index `-1`) is expanded to one offset per material at parse time, so
    /// this is never a sentinel.
    pub material_index: usize,
    /// How [`Self::base_color`] combines with the material's own value.
    pub operation: IrMaterialMorphOperation,
    /// Base color factor or addend, depending on [`Self::operation`].
    pub base_color: LinearRgba,
}

/// A secondary-motion rigid-body rig captured from a source (ADR 0097 §6).
///
/// Purely descriptive: it records the bodies and constraints a source's
/// author tuned, in engine units and axes, so a physics backend can build a
/// simulation from it. Nothing in this IR simulates anything.
#[derive(Debug, Clone, Default)]
pub struct IrRigidBodyRig {
    /// Bodies in source order; [`IrJoint`] entries index into this list.
    pub bodies: Vec<IrRigidBody>,
    /// Constraints between pairs of [`Self::bodies`].
    pub joints: Vec<IrJoint>,
}

/// The collision volume of one [`IrRigidBody`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IrRigidBodyShape {
    /// A sphere of the given radius, in engine units.
    Sphere {
        /// Sphere radius.
        radius: f32,
    },
    /// A box with the given half-extents, in engine units.
    Box {
        /// Half-extent on each local axis.
        half_extents: Vec3,
    },
    /// A capsule aligned with its body's local Y axis.
    Capsule {
        /// Cross-section radius.
        radius: f32,
        /// Half the length of the cylindrical section, excluding the caps.
        half_height: f32,
    },
}

/// How one [`IrRigidBody`] relates to the bone it is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IrRigidBodyMode {
    /// The body is driven by the bone: animation moves it, and it pushes
    /// other bodies without being pushed back. MMD calls this "static".
    #[default]
    FollowBone,
    /// The body simulates freely and drives its bone's transform. This is
    /// what makes hair and skirts move.
    Dynamic,
    /// The body simulates freely for rotation but keeps its bone's position,
    /// used where a chain must stay anchored while still swinging.
    DynamicWithBonePosition,
}

/// One rigid body in an [`IrRigidBodyRig`].
#[derive(Debug, Clone)]
pub struct IrRigidBody {
    /// Human-readable name from the source document.
    pub name: String,
    /// Index into [`ModelDocument::nodes`] of the bone this body is attached
    /// to, or `None` for a body attached to nothing (rare, but PMX permits
    /// it).
    pub bone_node: Option<usize>,
    /// Collision volume.
    pub shape: IrRigidBodyShape,
    /// The body's rest transform relative to its bone, in engine units and
    /// axes. Identity when the body sits exactly on its bone.
    pub bone_offset_translation: Vec3,
    /// Rotation of the body's local frame relative to its bone.
    pub bone_offset_rotation: Quat,
    /// Mass in kilograms. Ignored for [`IrRigidBodyMode::FollowBone`].
    pub mass: f32,
    /// Linear velocity damping factor.
    pub linear_damping: f32,
    /// Angular velocity damping factor.
    pub angular_damping: f32,
    /// Bounciness in `0..=1`.
    pub restitution: f32,
    /// Coulomb friction coefficient.
    pub friction: f32,
    /// How this body relates to its bone.
    pub mode: IrRigidBodyMode,
    /// Collision group index this body belongs to.
    pub group: u8,
    /// Bitmask of the groups this body collides with; bit *n* set means it
    /// collides with group *n*.
    pub collides_with: u16,
}

/// One constraint between two [`IrRigidBody`] entries.
///
/// Modeled as a six-degree-of-freedom spring, which is what MMD authors tune
/// and what every mainstream physics backend can express directly.
#[derive(Debug, Clone)]
pub struct IrJoint {
    /// Human-readable name from the source document.
    pub name: String,
    /// Index into [`IrRigidBodyRig::bodies`] of the first body, or `None`
    /// when the source referenced a body that does not exist.
    pub body_a: Option<usize>,
    /// Index into [`IrRigidBodyRig::bodies`] of the second body.
    pub body_b: Option<usize>,
    /// The constraint frame's position, in engine units and axes.
    pub translation: Vec3,
    /// The constraint frame's rotation.
    pub rotation: Quat,
    /// Lower translation limit per axis, in engine units.
    pub translation_lower: Vec3,
    /// Upper translation limit per axis, in engine units.
    pub translation_upper: Vec3,
    /// Lower rotation limit per axis, in radians.
    pub rotation_lower: Vec3,
    /// Upper rotation limit per axis, in radians.
    pub rotation_upper: Vec3,
    /// Per-axis translation spring stiffness; zero disables the spring.
    pub spring_translation: Vec3,
    /// Per-axis rotation spring stiffness; zero disables the spring.
    pub spring_rotation: Vec3,
}

/// One skin extracted from a [`ModelDocument`]'s source (ADR 0043, revised
/// by ADR 0077).
#[derive(Debug, Clone)]
pub struct IrSkin {
    /// Original zero-based selector in the source document.
    pub source_index: usize,
    /// Human-readable skin name.
    pub name: String,
    /// Indices into [`ModelDocument::nodes`], in skin joint order.
    pub joint_nodes: Vec<usize>,
    /// One inverse bind matrix per joint, same order as [`Self::joint_nodes`].
    pub inverse_bind_matrices: Vec<Mat4>,
}

/// One channel of a [`IrClip`], already resolved to a joint of the clip's
/// bound skin.
///
/// The IR stops short of a stable a stable runtime bone ID (which
/// requires skeleton construction and identity dedupe, a builder concern);
/// `joint_index` is the last format-independent step a parser can take.
#[derive(Debug, Clone)]
pub struct IrClipChannel {
    /// Position within the bound skin's [`IrSkin::joint_nodes`] this channel
    /// drives.
    pub joint_index: usize,
    /// Which transform property this channel drives.
    pub property: IrAnimProperty,
    /// Keyframe samples, already linear-interpolated (see the module
    /// normalization contract) and sorted by time ascending.
    pub keyframes: Vec<IrKeyframe>,
}

/// One animation clip extracted from a [`ModelDocument`]'s source.
///
/// Only clips with at least one channel bound to a joint of some skin
/// survive parsing; channels targeting a non-joint node, or a joint outside
/// the clip's first-resolved skin, are dropped with a diagnostic exactly
/// like today's glTF importer.
#[derive(Debug, Clone)]
pub struct IrClip {
    /// Original zero-based selector in the source document.
    pub source_index: usize,
    /// Human-readable clip name.
    pub name: String,
    /// Original selector of the skin every channel in this clip is bound to.
    ///
    /// Matches some [`IrSkin::source_index`].
    pub skin_index: usize,
    /// The clip's channels, already resolved to joints of the bound skin.
    pub channels: Vec<IrClipChannel>,
    /// Total clip length in seconds (the latest keyframe time among
    /// [`Self::channels`]).
    pub duration: f32,
}

/// One material converted from a [`ModelDocument`]'s source to the engine's
/// material contract.
#[derive(Debug, Clone)]
pub struct IrMaterial {
    /// Original zero-based selector in the source document.
    pub source_index: usize,
    /// Human-readable material name.
    pub name: String,
    /// Base color (albedo) multiplier.
    pub base_color: LinearRgba,
    /// Original selector of the base-color texture, if any and if it
    /// decoded successfully.
    pub base_color_texture: Option<usize>,
    /// Original selector of the tangent-space normal texture, if any and if
    /// it decoded successfully.
    pub normal_texture: Option<usize>,
    /// Original selector of the emissive texture, if any and if it decoded
    /// successfully.
    pub emissive_texture: Option<usize>,
    /// Linear HDR emissive multiplier.
    pub emissive_color: LinearRgba,
    /// Roughness in `[0, 1]`.
    pub roughness: f32,
    /// Metallic factor in `[0, 1]`.
    pub metallic: f32,
    /// Alpha coverage policy.
    pub alpha_mode: MaterialAlphaMode,
    /// Mask threshold used only by `alpha_mode = mask`.
    pub alpha_cutoff: f32,
    /// Triangle culling policy.
    pub cull_mode: MaterialCullMode,
    /// Lighting model.
    pub shading_model: MaterialShadingModel,
    /// Optional toon-ramp texture selector.
    pub toon_ramp_texture: Option<usize>,
    /// Toon dark-side color.
    pub toon_shadow_color: LinearRgba,
    /// Toon material-local ambient color.
    pub toon_ambient_color: LinearRgba,
    /// Toon compact-highlight color.
    pub toon_specular_color: LinearRgba,
    /// Toon compact-highlight exponent.
    pub toon_specular_power: f32,
    /// Optional sphere-map texture selector.
    pub sphere_texture: Option<usize>,
    /// Sphere-map blend operation.
    pub sphere_blend: MaterialSphereBlendMode,
    /// Sphere-map coordinate source.
    pub sphere_coordinates: MaterialSphereCoordinateSource,
    /// Independent outline-pass settings.
    pub outline: MaterialOutline,
    /// Whether this material casts scene shadows.
    pub cast_shadow: bool,
    /// Whether this material receives scene shadows.
    pub receive_shadow: bool,
}

/// One decoded texture extracted from a [`ModelDocument`]'s source.
#[derive(Debug, Clone)]
pub struct IrTexture {
    /// Original zero-based selector in the source document.
    pub source_index: usize,
    /// Human-readable texture name.
    pub name: String,
    /// Decoded pixel width.
    pub width: u32,
    /// Decoded pixel height.
    pub height: u32,
    /// Tightly packed RGBA8 pixels.
    pub rgba8: Vec<u8>,
}
