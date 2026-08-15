//! Engine-owned definitions for authorable runtime components.
//!
//! The registry in this module is the engine-side source of truth for
//! component schemas, editor hints, and authoring-to-runtime spawn callbacks.
//! Each built-in component is declared exactly once in the `builtins`
//! module; this
//! module owns the shared vocabulary those declarations are written in.

use crate::asset::{AssetManifest, ImportedSubAssetKind, ManifestEntry};
use crate::scene_bridge::{
    SceneBridgeError, ANIMATION_CONTROLLER_COMPONENT, AUDIO_EMITTER_COMPONENT,
    BUILTIN_BLUE_MATERIAL_ASSET_ID, BUILTIN_ORANGE_MATERIAL_ASSET_ID, BUILTIN_QUAD_ASSET_ID,
    BUILTIN_TRIANGLE_ASSET_ID, BUILTIN_UI_DOCUMENT_ASSET_ID, BUILTIN_WHITE_MATERIAL_ASSET_ID,
    CAMERA_COMPONENT, CHARACTER_CONTROLLER_COMPONENT, FOOT_IK_COMPONENT, LOD_GROUP_COMPONENT,
    PARTICLE_EMITTER_COMPONENT, RUNTIME_METADATA_COMPONENT, SHADOW_SETTINGS_COMPONENT,
    SKINNED_MESH_RENDERER_COMPONENT, SKINNED_MODEL_COMPONENT, STATIC_MESH_RENDERER_COMPONENT,
    TRANSFORM_COMPONENT,
};
use engine_authoring::id::{AssetId, ComponentTypeId, EntityId};
use engine_authoring::schema::{ComponentSchema, FieldType};
use engine_authoring::value::Value;
use engine_authoring::{AuthoringEntity, AuthoringScene, Diagnostic, DiagnosticTarget};
use engine_ecs::{Entity, World};
use hashbrown::HashMap;
use std::collections::BTreeMap;
use std::path::Path;

/// Asset category used by component inspector metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    /// A CPU or GPU mesh asset.
    Mesh,
    /// A material asset.
    Material,
    /// A texture image used by a material.
    Texture,
    /// An animation clip or source containing importable clips.
    AnimationClip,
    /// A compiled-at-load animation state-machine graph.
    AnimationGraph,
    /// An author-owned mapping from animation-graph motion slots to imported
    /// animation-clip sub-assets.
    AnimationSet,
    /// A compiled-at-load Behavior Tree graph.
    BehaviorTree,
    /// A decoded sound effect or music asset.
    Audio,
    /// A baked navigation mesh artifact.
    NavMesh,
    /// A declarative UI document asset (Phase 54 / ADR 0046).
    UiDocument,
    /// A reusable authoring prefab document.
    Prefab,
    /// A glTF/GLB, FBX, or PMX source document used for mesh, skin, or
    /// animation import (ADR 0081 widened this from glTF/GLB-only, ADR 0097
    /// widened it again to PMX; the variant name is a pre-existing misnomer
    /// left unchanged, matching `GltfImportResult`).
    GltfSource,
    /// A `*.vmd` motion document: an animation-only source that carries no
    /// mesh, material, or skeleton of its own (ADR 0097 §3).
    ///
    /// Distinct from [`Self::GltfSource`] because importing one needs a
    /// second input — either each direct PMX bake target or an optional
    /// original PMX followed by explicit retargeting, recorded in
    /// `crate::asset::ImportSettings` — and
    /// because it produces only Animation sub-assets, never a mesh or a
    /// placement prefab.
    MotionSource,
    /// A skin (joint list plus inverse-bind matrices) imported from a glTF
    /// source. Skins exist only as imported sub-assets, never as files.
    Skin,
    /// A skeleton (bone hierarchy and rest pose) imported from a model
    /// source. Like [`Self::Skin`], skeletons exist only as imported
    /// sub-assets, never as files (ADR 0077, ADR 0087).
    Skeleton,
    /// A named vertex/material deformation imported from a model source
    /// (ADR 0097 §5). Like [`Self::Skin`], morphs exist only as imported
    /// sub-assets.
    Morph,
    /// A secondary-motion rigid-body rig imported from a model source
    /// (ADR 0097 §6). Like [`Self::Skin`], rigs exist only as imported
    /// sub-assets, never as files.
    RigidBodyRig,
}

/// Returns whether a manifest path can represent the requested asset category.
///
/// Graph categories additionally require their persisted `kind` value to be
/// checked by the caller because a `.graph.json` suffix alone is ambiguous.
pub fn asset_path_matches_kind(kind: AssetKind, path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match kind {
        AssetKind::Mesh => extension_matches(extension, &["obj", "gltf", "glb", "fbx", "pmx"]),
        AssetKind::Material => file_name.ends_with(".material.json"),
        AssetKind::Texture => extension_matches(extension, &["png", "jpg", "jpeg", "webp", "bmp"]),
        AssetKind::AnimationClip => extension_matches(extension, &["gltf", "glb", "fbx", "vmd"]),
        AssetKind::AnimationGraph | AssetKind::BehaviorTree => file_name.ends_with(".graph.json"),
        AssetKind::AnimationSet => file_name.ends_with(".animset.json"),
        AssetKind::Audio => extension_matches(extension, &["wav", "ogg"]),
        AssetKind::NavMesh => file_name == "navmesh.bin" || file_name.ends_with(".navmesh.json"),
        AssetKind::UiDocument => file_name.ends_with(".ui.json"),
        AssetKind::Prefab => file_name.ends_with(".prefab.json"),
        AssetKind::GltfSource => extension_matches(extension, &["gltf", "glb", "fbx", "pmx"]),
        AssetKind::MotionSource => extension_matches(extension, &["vmd"]),
        // Skins, skeletons, morphs, and rigid-body rigs are only ever
        // imported sub-assets of a model source, so no standalone path can
        // represent one.
        AssetKind::Skin
        | AssetKind::Skeleton
        | AssetKind::Morph
        | AssetKind::RigidBodyRig => false,
    }
}

fn extension_matches(extension: &str, expected: &[&str]) -> bool {
    expected
        .iter()
        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
}

/// Presentation metadata for tools that edit a component value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InspectorHint {
    /// Use generic editors derived from the component schema.
    Default,
    /// The component value is an asset reference of the given kind.
    AssetRef {
        /// The asset category accepted by the component.
        kind: AssetKind,
    },
    /// Field-level controls taken from the component's declaration table.
    Fields {
        /// The component's declared fields, looked up by schema field name.
        fields: &'static [FieldDef],
    },
}

/// Editor control used for one schema field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InspectorFieldControl {
    /// Select one of the listed serialized string values.
    Enum(&'static [&'static str]),
    /// Select a manifest asset filtered to the declared category.
    AssetRef(AssetKind),
    /// Select a bone of the rig named by a sibling entity-reference field.
    ///
    /// The control offers bone names and stores the `BoneId` behind them, so
    /// an author never types an ID and a persisted binding is never a name
    /// (ADR 0077, ADR 0088 1).
    BoneRef {
        /// Sibling field naming the entity whose rig supplies the bones.
        rig_field: &'static str,
    },
    /// Select a scene entity that carries every listed component type.
    ///
    /// The Inspector offers only matching entities and validation reports a
    /// stored reference to a non-matching entity, so a wrong target cannot be
    /// produced through the UI and cannot survive unnoticed when produced by
    /// hand-edited JSON or a tool (ADR 0087 §4). An empty list accepts any
    /// entity.
    EntityRef(&'static [&'static str]),
    /// Edit an ordered list of manifest assets filtered to one category.
    AssetRefList(AssetKind),
    /// Select bits using project collision-layer names.
    LayerMask,
    /// Edit a number while exposing its accepted semantic range.
    Number(NumericRange),
    /// Edit the structured distance/mesh rows of `engine.lod_group`.
    LodLevels,
    /// Edit a deterministic string-to-boolean parameter map.
    StringBoolMap,
}

/// Inclusive or exclusive numeric bounds shared by Inspector and validation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericRange {
    /// Optional lower bound.
    pub min: Option<f64>,
    /// Whether `min` itself is accepted.
    pub min_inclusive: bool,
    /// Optional upper bound.
    pub max: Option<f64>,
    /// Whether `max` itself is accepted.
    pub max_inclusive: bool,
}

impl NumericRange {
    /// Creates a range with an inclusive lower bound and no upper bound.
    pub const fn at_least(min: f64) -> Self {
        Self {
            min: Some(min),
            min_inclusive: true,
            max: None,
            max_inclusive: true,
        }
    }

    /// Creates a range with an exclusive lower bound and no upper bound.
    pub const fn greater_than(min: f64) -> Self {
        Self {
            min: Some(min),
            min_inclusive: false,
            max: None,
            max_inclusive: true,
        }
    }

    /// Creates an inclusive closed range.
    pub const fn inclusive(min: f64, max: f64) -> Self {
        Self {
            min: Some(min),
            min_inclusive: true,
            max: Some(max),
            max_inclusive: true,
        }
    }

    /// Returns whether a finite value satisfies both configured bounds.
    pub fn contains(self, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        let lower_ok = self.min.is_none_or(|min| {
            if self.min_inclusive {
                value >= min
            } else {
                value > min
            }
        });
        let upper_ok = self.max.is_none_or(|max| {
            if self.max_inclusive {
                value <= max
            } else {
                value < max
            }
        });
        lower_ok && upper_ok
    }

    /// Produces a compact message suitable for field-level validation UI.
    pub fn expectation(self) -> String {
        match (self.min, self.max) {
            (Some(min), Some(max)) => format!(
                "must be {} {min} and {} {max}",
                if self.min_inclusive {
                    "at least"
                } else {
                    "greater than"
                },
                if self.max_inclusive {
                    "at most"
                } else {
                    "less than"
                }
            ),
            (Some(min), None) => format!(
                "must be {} {min}",
                if self.min_inclusive {
                    "at least"
                } else {
                    "greater than"
                }
            ),
            (None, Some(max)) => format!(
                "must be {} {max}",
                if self.max_inclusive {
                    "at most"
                } else {
                    "less than"
                }
            ),
            (None, None) => "must be finite".to_owned(),
        }
    }
}

/// Simple condition controlling whether one field is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorFieldCondition {
    /// Show when another boolean field equals this value.
    Bool {
        /// Controlling sibling field.
        field: &'static str,
        /// Required boolean value.
        equals: bool,
    },
    /// Show when another string field equals this value.
    String {
        /// Controlling sibling field.
        field: &'static str,
        /// Required serialized string.
        equals: &'static str,
    },
    /// Show only when a field already holds a non-null value.
    ///
    /// Used for deprecated reference fields: content that still carries one
    /// stays inspectable and repairable, while new content is never offered
    /// the field in the first place (ADR 0087 §3).
    Assigned {
        /// Controlling field, which may be the hinted field itself.
        field: &'static str,
    },
    /// Show when another string field matches any listed value.
    StringAny {
        /// Controlling sibling field.
        field: &'static str,
        /// Accepted serialized strings.
        values: &'static [&'static str],
    },
}

/// Error returned while applying an authoring component to a runtime entity.
#[derive(Debug)]
pub enum ComponentSpawnError {
    /// The authoring component value was invalid for this component type.
    Bridge(SceneBridgeError),
    /// Runtime ECS mutation failed while adding or updating a component.
    World(engine_ecs::WorldError),
}

impl From<SceneBridgeError> for ComponentSpawnError {
    fn from(error: SceneBridgeError) -> Self {
        Self::Bridge(error)
    }
}

impl From<engine_ecs::WorldError> for ComponentSpawnError {
    fn from(error: engine_ecs::WorldError) -> Self {
        Self::World(error)
    }
}

/// Mutable context passed to a component spawn callback.
///
/// The context owns bridge-local state for the current conversion, including
/// diagnostics and runtime asset handles that must be rolled back if a later
/// component fails.
pub struct SpawnContext<'a> {
    pub(crate) world: &'a mut World,
    pub(crate) authoring_entity: &'a AuthoringEntity,
    pub(crate) asset_root: Option<&'a Path>,
    pub(crate) manifest: &'a AssetManifest,
    /// Complete authoring-to-runtime entity map for this conversion pass.
    ///
    /// All entities are pre-allocated before any component spawn callbacks are
    /// invoked, so entity references can be resolved in any order.
    pub(crate) entity_map: &'a HashMap<EntityId, Entity>,
    pub(crate) asset_diagnostics: &'a mut Vec<Diagnostic>,
    /// All conversion-local asset caches and rollback ownership.
    pub(crate) asset_state: &'a mut crate::scene_bridge::BridgeAssetState,
}

/// Function that applies one authoring component value to a runtime entity.
pub type SpawnFn =
    for<'a> fn(Entity, &Value, &mut SpawnContext<'a>) -> Result<(), ComponentSpawnError>;

/// Complete engine definition for one authorable component.
#[derive(Clone)]
pub struct ComponentDefinition {
    /// Authoring schema for validation, defaults, and generated editors.
    pub schema: ComponentSchema,
    /// Runtime spawn callback for this component.
    pub spawn: SpawnFn,
    /// Tool-facing inspector metadata for this component.
    pub inspector: InspectorHint,
    /// Whether editors should present this component collapsed by default.
    ///
    /// Declared once per component in the built-in table (ADR 0102) so the
    /// editor does not carry a second list of which components are long.
    pub default_collapsed: bool,
}

/// Engine-owned registry of authorable component definitions.
#[derive(Clone, Default)]
pub struct ComponentRegistry {
    definitions: BTreeMap<ComponentTypeId, ComponentDefinition>,
    order: Vec<ComponentTypeId>,
}

const NON_NEGATIVE: NumericRange = NumericRange::at_least(0.0);
const POSITIVE: NumericRange = NumericRange::greater_than(0.0);
const UNIT_INTERVAL: NumericRange = NumericRange::inclusive(0.0, 1.0);
const U32_RANGE: NumericRange = NumericRange::inclusive(0.0, u32::MAX as f64);


impl ComponentRegistry {
    /// Creates an empty component registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a definition, replacing any existing definition with the same type id.
    pub fn register(&mut self, definition: ComponentDefinition) {
        let type_id = definition.schema.type_id.clone();
        if !self.definitions.contains_key(&type_id) {
            self.order.push(type_id.clone());
        }
        self.definitions.insert(type_id, definition);
    }

    /// Returns the definition for `component_type`, if one is registered.
    pub fn get(&self, component_type: &ComponentTypeId) -> Option<&ComponentDefinition> {
        self.definitions.get(component_type)
    }

    /// Iterates definitions in deterministic registration order.
    pub fn definitions(&self) -> impl Iterator<Item = &ComponentDefinition> {
        self.order
            .iter()
            .filter_map(|component_type| self.definitions.get(component_type))
    }

    /// Returns the number of registered definitions.
    pub fn len(&self) -> usize {
        self.definitions.len()
    }

    /// Returns `true` when no definitions are registered.
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }
}

/// Builds the engine registry for built-in authorable components.
///
/// Every definition comes from the single declaration table in the private
/// `builtins` module, so the registry, the schemas, and the Inspector metadata
/// cannot disagree
/// about a component, and the set of built-ins is never stated twice.
pub fn builtin_registry() -> ComponentRegistry {
    let mut registry = ComponentRegistry::new();
    for component in builtins::builtin_components() {
        registry.register(ComponentDefinition {
            schema: component.schema(),
            inspector: component.inspector(),
            spawn: component.spawn,
            default_collapsed: component.default_collapsed,
        });
    }
    registry
}

mod builtins;
mod definition;
mod schemas;
#[cfg(test)]
mod tests;
mod validation;

pub use definition::{BuiltinComponent, FieldDef, FieldDefaultSpec, FieldKind};
pub use validation::*;
