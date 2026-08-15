//! Prefab asset model: save-as-prefab and instantiate operations (Phase 33).
//!
//! A prefab stores an entity subtree as a reusable `.prefab.json` asset
//! following ADR 0030.  On every instantiation, all stored `EntityId` values
//! are remapped to freshly generated IDs so that multiple instances cannot
//! collide.

use crate::entity::AuthoringEntity;
use crate::id::EntityId;
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Schema version written into every `.prefab.json` file.
///
/// `PrefabAsset::from_json` rejects files whose `schema_version` exceeds this
/// value.
pub const PREFAB_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A reusable entity-subtree template stored as `.prefab.json` (ADR 0030).
///
/// Entity IDs inside the prefab are **prefab-local** stable identifiers.  They
/// must never be inserted into a live scene directly; call
/// [`PrefabAsset::instantiate`] to obtain a fresh set of
/// [`AuthoringCommand`] values with new scene-unique IDs.
///
/// [`AuthoringCommand`]: crate::command::AuthoringCommand
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefabAsset {
    /// Schema version for forward-compatibility detection (ADR 0020 pattern).
    pub schema_version: u32,
    /// The prefab-local ID of the root entity in the stored subtree.
    pub root: EntityId,
    /// All entities in the subtree keyed by their prefab-local IDs.
    ///
    /// The `parent` field of each entry uses prefab-local IDs.  Root entity
    /// has `parent: None`.
    pub entities: BTreeMap<EntityId, AuthoringEntity>,
}

/// Commands and remapped root ID produced by one prefab instantiation.
#[derive(Debug)]
pub struct PrefabInstantiation {
    /// Commands that create the remapped prefab subtree.
    pub commands: Vec<crate::command::AuthoringCommand>,
    /// Fresh authoring ID assigned to the prefab root.
    pub root: EntityId,
}

/// Describes why a [`PrefabAsset`] operation failed.
#[derive(Debug)]
pub enum PrefabError {
    /// The JSON could not be parsed.
    Json(serde_json::Error),
    /// The prefab file uses a schema version newer than this build supports.
    UnsupportedVersion {
        /// The version number found in the file.
        found: u32,
    },
    /// The declared root entity is absent from the `entities` map.
    RootNotFound(EntityId),
    /// A `parent` reference inside the prefab points to an entity not in the
    /// `entities` map.
    OrphanedParentRef {
        /// The entity that carries the dangling reference.
        entity: EntityId,
        /// The referenced parent ID that is absent.
        parent: EntityId,
    },
    /// The entity selection is empty; at least one entity is required.
    EmptySelection,
    /// No root entity could be identified (all selected entities have a parent
    /// inside the selection).
    NoRoot,
    /// More than one candidate root was found; a prefab must have exactly one.
    MultipleRoots,
}

impl fmt::Display for PrefabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "prefab JSON error: {e}"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "prefab schema_version {found} is not supported (max: {PREFAB_SCHEMA_VERSION})"
            ),
            Self::RootNotFound(id) => write!(f, "root entity `{id}` not found in prefab"),
            Self::OrphanedParentRef { entity, parent } => write!(
                f,
                "entity `{entity}` references parent `{parent}` which is not in the prefab"
            ),
            Self::EmptySelection => write!(f, "cannot create a prefab from an empty selection"),
            Self::NoRoot => write!(
                f,
                "selection has no root entity (cycle or all have parents in selection)"
            ),
            Self::MultipleRoots => write!(
                f,
                "selection has multiple root candidates; a prefab must have exactly one root"
            ),
        }
    }
}

impl std::error::Error for PrefabError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// PrefabAsset implementation
// ---------------------------------------------------------------------------

impl PrefabAsset {
    /// Builds a [`PrefabAsset`] from a selection of entities.
    ///
    /// `selection` is an iterator of `(id, entity)` pairs taken from
    /// `crate::scene::AuthoringScene::entities`.  The method identifies the single root
    /// (the entity whose `parent` is absent or not in the selection) and
    /// validates that the selection forms a proper subtree.
    ///
    /// # Errors
    ///
    /// - [`PrefabError::EmptySelection`] when the iterator is empty.
    /// - [`PrefabError::NoRoot`] when every entity has a parent inside the
    ///   selection (cycle detected).
    /// - [`PrefabError::MultipleRoots`] when more than one entity has no
    ///   in-selection parent.
    pub fn from_selection<'a>(
        selection: impl IntoIterator<Item = (&'a EntityId, &'a AuthoringEntity)>,
    ) -> Result<Self, PrefabError> {
        let entities: BTreeMap<EntityId, AuthoringEntity> = selection
            .into_iter()
            .map(|(id, e)| (id.clone(), e.clone()))
            .collect();

        if entities.is_empty() {
            return Err(PrefabError::EmptySelection);
        }

        let ids: BTreeSet<&EntityId> = entities.keys().collect();

        let roots: Vec<&EntityId> = entities
            .iter()
            .filter(|(_, e)| e.parent.as_ref().map(|p| !ids.contains(p)).unwrap_or(true))
            .map(|(id, _)| id)
            .collect();

        let root = match roots.as_slice() {
            [] => return Err(PrefabError::NoRoot),
            [r] => (*r).clone(),
            _ => return Err(PrefabError::MultipleRoots),
        };

        Ok(Self {
            schema_version: PREFAB_SCHEMA_VERSION,
            root,
            entities,
        })
    }

    /// Parses a `.prefab.json` string.
    ///
    /// # Errors
    ///
    /// - [`PrefabError::Json`] when the JSON is malformed.
    /// - [`PrefabError::UnsupportedVersion`] when `schema_version` exceeds
    ///   [`PREFAB_SCHEMA_VERSION`].
    /// - [`PrefabError::RootNotFound`] when the declared root is absent.
    pub fn from_json(json: &str) -> Result<Self, PrefabError> {
        let asset: PrefabAsset = serde_json::from_str(json).map_err(PrefabError::Json)?;
        if asset.schema_version != PREFAB_SCHEMA_VERSION {
            return Err(PrefabError::UnsupportedVersion {
                found: asset.schema_version,
            });
        }
        if !asset.entities.contains_key(&asset.root) {
            return Err(PrefabError::RootNotFound(asset.root.clone()));
        }
        for (id, entity) in &asset.entities {
            if let Some(parent) = &entity.parent
                && !asset.entities.contains_key(parent)
            {
                return Err(PrefabError::OrphanedParentRef {
                    entity: id.clone(),
                    parent: parent.clone(),
                });
            }
        }
        Ok(asset)
    }

    /// Serializes this prefab to canonical pretty-printed JSON.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if serialization fails (should never
    /// happen for structurally valid prefabs).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Produces a list of `AuthoringCommand` values that instantiate this
    /// prefab into a live scene.
    ///
    /// Every entity ID in the prefab is remapped to a freshly generated
    /// [`EntityId`] so that multiple instantiations cannot collide.  The
    /// caller is responsible for applying the returned commands through a
    /// [`Transaction`].
    ///
    /// `parent_override` replaces the root entity's parent field; use `None`
    /// to create the root at scene level.
    ///
    /// [`Transaction`]: crate::transaction::Transaction
    pub fn instantiate(
        &self,
        parent_override: Option<EntityId>,
    ) -> Result<Vec<crate::command::AuthoringCommand>, PrefabError> {
        Ok(self.instantiate_with_root(parent_override)?.commands)
    }

    /// Produces prefab commands and the freshly remapped root ID.
    ///
    /// This has the same validation and remapping behavior as
    /// [`PrefabAsset::instantiate`], while exposing the new root ID to runtime
    /// bridges that need to position or report the spawned subtree.
    ///
    /// # Errors
    ///
    /// Returns the same structural errors as [`PrefabAsset::instantiate`].
    pub fn instantiate_with_root(
        &self,
        parent_override: Option<EntityId>,
    ) -> Result<PrefabInstantiation, PrefabError> {
        let id_map: BTreeMap<EntityId, EntityId> = self
            .entities
            .keys()
            .map(|old_id| (old_id.clone(), EntityId::generate()))
            .collect();
        let root = id_map
            .get(&self.root)
            .cloned()
            .ok_or_else(|| PrefabError::RootNotFound(self.root.clone()))?;

        let mut commands = Vec::new();

        for (old_id, entity) in &self.entities {
            let new_id = id_map[old_id].clone();

            let parent = if old_id == &self.root {
                parent_override.clone()
            } else {
                entity
                    .parent
                    .as_ref()
                    .map(|p| {
                        id_map
                            .get(p)
                            .cloned()
                            .ok_or_else(|| PrefabError::OrphanedParentRef {
                                entity: old_id.clone(),
                                parent: p.clone(),
                            })
                    })
                    .transpose()?
            };

            commands.push(crate::command::AuthoringCommand::CreateEntity {
                id: new_id.clone(),
                name: entity.name.clone(),
                parent,
            });

            for (component_type, value) in &entity.components {
                commands.push(crate::command::AuthoringCommand::AddComponent {
                    entity: new_id.clone(),
                    component_type: component_type.clone(),
                    value: remap_entity_refs(value, &id_map),
                });
            }
        }

        Ok(PrefabInstantiation { commands, root })
    }
}

/// Rewrites every [`Value::EntityRef`] in `value` through `id_map`.
///
/// Component values may reference sibling entities of the same prefab (for
/// example `engine.skinned_mesh.skeleton`). Those references store
/// prefab-local IDs, so an instance that kept them verbatim would point at
/// entities that do not exist in the scene.
///
/// References with no entry in `id_map` point outside the prefab and are left
/// unchanged, so scene validation reports `scene.bad_entity_ref` instead of
/// this function silently substituting a different entity.
fn remap_entity_refs(value: &Value, id_map: &BTreeMap<EntityId, EntityId>) -> Value {
    match value {
        Value::EntityRef(id) => {
            Value::EntityRef(id_map.get(id).cloned().unwrap_or_else(|| id.clone()))
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| remap_entity_refs(item, id_map))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, field)| (name.clone(), remap_entity_refs(field, id_map)))
                .collect(),
        ),
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ComponentTypeId;
    use std::collections::BTreeMap;

    fn entity_with_parent(name: &str, parent: Option<EntityId>) -> (EntityId, AuthoringEntity) {
        let id = EntityId::generate();
        let mut e = AuthoringEntity::new(id.clone(), name);
        e.parent = parent;
        (id, e)
    }

    #[test]
    fn from_selection_single_entity_is_root() {
        let (id, entity) = entity_with_parent("player", None);
        let prefab = PrefabAsset::from_selection([(&id, &entity)]).expect("must succeed");
        assert_eq!(prefab.root, id);
        assert_eq!(prefab.entities.len(), 1);
    }

    #[test]
    fn instantiate_remaps_entity_references_between_prefab_members() {
        let (root_id, root) = entity_with_parent("character", None);
        let (mesh_id, mut mesh) = entity_with_parent("body", Some(root_id.clone()));
        let mut skinned = BTreeMap::new();
        skinned.insert("skeleton".to_owned(), Value::EntityRef(root_id.clone()));
        mesh.components.insert(
            ComponentTypeId::new("engine.skinned_mesh"),
            Value::Object(skinned),
        );
        let prefab = PrefabAsset::from_selection([(&root_id, &root), (&mesh_id, &mesh)])
            .expect("must succeed");

        let instantiation = prefab
            .instantiate_with_root(None)
            .expect("instantiation must succeed");

        let reference = instantiation
            .commands
            .iter()
            .find_map(|command| match command {
                crate::command::AuthoringCommand::AddComponent {
                    value: Value::Object(fields),
                    ..
                } => fields.get("skeleton").cloned(),
                _ => None,
            })
            .expect("skinned mesh component must be instantiated");
        assert_eq!(
            reference,
            Value::EntityRef(instantiation.root.clone()),
            "an intra-prefab entity reference must follow the remapped root"
        );
        assert_ne!(reference, Value::EntityRef(root_id));
    }

    #[test]
    fn instantiate_keeps_entity_references_that_leave_the_prefab() {
        let (root_id, root) = entity_with_parent("character", None);
        let outside = EntityId::generate();
        let mut root = root;
        let mut component = BTreeMap::new();
        component.insert("source".to_owned(), Value::EntityRef(outside.clone()));
        root.components.insert(
            ComponentTypeId::new("engine.lock_on_camera"),
            Value::Object(component),
        );
        let prefab = PrefabAsset::from_selection([(&root_id, &root)]).expect("must succeed");

        let instantiation = prefab
            .instantiate_with_root(None)
            .expect("instantiation must succeed");

        let reference = instantiation
            .commands
            .iter()
            .find_map(|command| match command {
                crate::command::AuthoringCommand::AddComponent {
                    value: Value::Object(fields),
                    ..
                } => fields.get("source").cloned(),
                _ => None,
            })
            .expect("component must be instantiated");
        assert_eq!(reference, Value::EntityRef(outside));
    }

    #[test]
    fn instantiate_with_root_returns_the_remapped_root() {
        let (id, entity) = entity_with_parent("player", None);
        let prefab = PrefabAsset::from_selection([(&id, &entity)]).expect("must succeed");

        let instantiation = prefab
            .instantiate_with_root(None)
            .expect("instantiation must succeed");

        assert_ne!(instantiation.root, id);
        assert!(matches!(
            instantiation.commands.first(),
            Some(crate::command::AuthoringCommand::CreateEntity { id, .. })
                if id == &instantiation.root
        ));
    }

    #[test]
    fn from_selection_parent_child_pair_identifies_root() {
        let (parent_id, parent_entity) = entity_with_parent("root", None);
        let (child_id, child_entity) = entity_with_parent("child", Some(parent_id.clone()));
        let prefab =
            PrefabAsset::from_selection([(&parent_id, &parent_entity), (&child_id, &child_entity)])
                .expect("must succeed");
        assert_eq!(prefab.root, parent_id);
    }

    #[test]
    fn from_selection_empty_returns_error() {
        let entities: Vec<(&EntityId, &AuthoringEntity)> = Vec::new();
        assert!(matches!(
            PrefabAsset::from_selection(entities),
            Err(PrefabError::EmptySelection)
        ));
    }

    #[test]
    fn from_selection_two_roots_returns_error() {
        let (id_a, entity_a) = entity_with_parent("a", None);
        let (id_b, entity_b) = entity_with_parent("b", None);
        assert!(matches!(
            PrefabAsset::from_selection([(&id_a, &entity_a), (&id_b, &entity_b)]),
            Err(PrefabError::MultipleRoots)
        ));
    }

    #[test]
    fn prefab_json_roundtrip_preserves_all_fields() {
        let (id, entity) = entity_with_parent("hero", None);
        let prefab = PrefabAsset::from_selection([(&id, &entity)]).expect("must succeed");
        let json = prefab.to_json().expect("must serialize");
        let parsed = PrefabAsset::from_json(&json).expect("must parse");
        assert_eq!(parsed, prefab);
    }

    #[test]
    fn from_json_rejects_unsupported_version() {
        let (id, entity) = entity_with_parent("hero", None);
        let mut prefab = PrefabAsset::from_selection([(&id, &entity)]).expect("must succeed");
        prefab.schema_version = 99;
        let json = serde_json::to_string(&prefab).expect("must serialize");
        assert!(matches!(
            PrefabAsset::from_json(&json),
            Err(PrefabError::UnsupportedVersion { found: 99 })
        ));
    }

    #[test]
    fn instantiate_generates_unique_entity_ids() {
        let (id, entity) = entity_with_parent("hero", None);
        let prefab = PrefabAsset::from_selection([(&id, &entity)]).expect("must succeed");

        let cmds1 = prefab.instantiate(None).expect("must succeed");
        let cmds2 = prefab.instantiate(None).expect("must succeed");

        let id1 = match &cmds1[0] {
            crate::command::AuthoringCommand::CreateEntity { id, .. } => id.clone(),
            _ => panic!("first command must be CreateEntity"),
        };
        let id2 = match &cmds2[0] {
            crate::command::AuthoringCommand::CreateEntity { id, .. } => id.clone(),
            _ => panic!("first command must be CreateEntity"),
        };

        assert_ne!(
            id1, id2,
            "each instantiation must generate a unique entity ID"
        );
    }

    #[test]
    fn instantiate_preserves_component_values() {
        let (id, mut entity) = entity_with_parent("hero", None);
        entity.components.insert(
            ComponentTypeId::new("engine.transform"),
            Value::Object(BTreeMap::from([
                ("x".into(), Value::F64(1.0)),
                ("y".into(), Value::F64(2.0)),
                ("z".into(), Value::F64(3.0)),
            ])),
        );
        let prefab = PrefabAsset::from_selection([(&id, &entity)]).expect("must succeed");
        let cmds = prefab.instantiate(None).expect("must succeed");

        let add_cmd = cmds.iter().find(|c| {
            matches!(c, crate::command::AuthoringCommand::AddComponent { component_type, .. }
                if component_type.as_str() == "engine.transform")
        });
        assert!(
            add_cmd.is_some(),
            "transform component command must be present"
        );
    }

    #[test]
    fn instantiate_with_parent_override_sets_root_parent() {
        let (id, entity) = entity_with_parent("hero", None);
        let prefab = PrefabAsset::from_selection([(&id, &entity)]).expect("must succeed");

        let scene_parent = EntityId::generate();
        let cmds = prefab
            .instantiate(Some(scene_parent.clone()))
            .expect("must succeed");

        let root_create = &cmds[0];
        if let crate::command::AuthoringCommand::CreateEntity { parent, .. } = root_create {
            assert_eq!(parent.as_ref(), Some(&scene_parent));
        } else {
            panic!("first command must be CreateEntity for root");
        }
    }

    #[test]
    fn instantiate_remaps_parent_references() {
        let (parent_id, parent_entity) = entity_with_parent("root", None);
        let (child_id, child_entity) = entity_with_parent("child", Some(parent_id.clone()));

        let prefab =
            PrefabAsset::from_selection([(&parent_id, &parent_entity), (&child_id, &child_entity)])
                .expect("must succeed");

        let cmds = prefab.instantiate(None).expect("must succeed");

        let creates: Vec<_> = cmds
            .iter()
            .filter_map(|c| {
                if let crate::command::AuthoringCommand::CreateEntity { id, name, parent } = c {
                    Some((id.clone(), name.clone(), parent.clone()))
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(creates.len(), 2, "two CreateEntity commands expected");

        let root_new_id = creates
            .iter()
            .find(|(_, name, _)| name == "root")
            .map(|(id, _, _)| id.clone())
            .expect("root create must exist");

        let child_parent = creates
            .iter()
            .find(|(_, name, _)| name == "child")
            .and_then(|(_, _, p)| p.clone())
            .expect("child must have a parent after remap");

        assert_eq!(
            child_parent, root_new_id,
            "child parent must be remapped to the new root ID"
        );
        assert_ne!(
            child_parent, parent_id,
            "child must NOT use the original prefab-local parent ID"
        );
    }
}
