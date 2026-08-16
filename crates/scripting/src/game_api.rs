//! Typed project-gameplay API layered over the versioned module ABI.
//!
//! The native module boundary deliberately transfers stable IDs and serialized
//! values. This module keeps that representation behind typed system
//! parameters so ordinary game logic never has to inspect maps, field names,
//! array offsets, or [`engine_authoring::Value`] variants.

use crate::game_io::{
    EngineViewKind, GameAccessMode, GameBehaviorStatus, GameClock, GameCommand, GameCommandFamily,
    GameComponentAccess, GameEngineViewAccess, GameEntityHandle, GameEventEmission, GameEventStream,
    GameHitboxShape, GameHostViewKind, GameInvocation, GameInvocationOutput, GameQueryAccess,
    GameQueryRow, GameResourceAccess, GameResourcePatch, GameSpatialAudioOptions, GameSystemAccess,
};
use crate::game_contracts::{GameComponent, GameField, GameResource};
use engine_authoring::{ComponentTypeId, EntityId, StableId, Value};
use glam::{Quat, Vec2, Vec3};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

mod sealed {
    pub trait SystemParam {}
}

/// Shared callback output used by generated typed-system adapters.
///
/// The type is public only because procedural-macro expansion occurs in the
/// project crate. Game systems receive higher-level wrappers instead.
#[doc(hidden)]
#[derive(Clone)]
pub struct TypedOutput(Rc<RefCell<GameInvocationOutput>>);

impl TypedOutput {
    /// Creates an empty typed callback output.
    #[doc(hidden)]
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(GameInvocationOutput::default())))
    }

    /// Extracts the completed ABI output after all typed parameters are dropped.
    #[doc(hidden)]
    pub fn into_output(self) -> Result<GameInvocationOutput, GameApiError> {
        Rc::try_unwrap(self.0)
            .map(RefCell::into_inner)
            .map_err(|_| GameApiError::Internal("typed system parameters outlived the callback"))
    }

    fn with_mut<R>(&self, mutate: impl FnOnce(&mut GameInvocationOutput) -> R) -> R {
        mutate(&mut self.0.borrow_mut())
    }
}

/// Reports invalid, missing, or undeclared data at the game API boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameApiError {
    /// An expected input action was omitted from the host invocation.
    MissingInputAction(&'static str),
    /// A declared save value could not be decoded as its key's Rust type.
    InvalidSaveValue {
        /// Declared save key.
        key: &'static str,
        /// Decode failure.
        reason: String,
    },
    /// A required project resource was omitted from the host invocation.
    MissingResource(&'static str),
    /// A project resource could not be decoded as its declared Rust type.
    InvalidResource {
        /// Stable resource ID.
        id: &'static str,
        /// Decode failure.
        reason: String,
    },
    /// A declared query result was omitted from the host invocation.
    MissingQuery(&'static str),
    /// Game code requested a component that its query specification did not declare.
    UndeclaredComponent {
        /// Query whose declaration was checked.
        query: &'static str,
        /// Stable component type ID.
        component: &'static str,
    },
    /// Game code tried to patch a component that was not declared writable.
    ReadOnlyComponent {
        /// Query whose declaration was checked.
        query: &'static str,
        /// Stable component type ID.
        component: &'static str,
    },
    /// A required component was absent from one matching row.
    MissingComponent {
        /// Query containing the row.
        query: &'static str,
        /// Stable component type ID.
        component: &'static str,
        /// Row entity.
        entity: GameEntityHandle,
    },
    /// A component value did not match the Rust component schema.
    InvalidComponent {
        /// Query containing the row.
        query: &'static str,
        /// Stable component type ID.
        component: &'static str,
        /// Row entity.
        entity: GameEntityHandle,
        /// Schema decode failure.
        reason: String,
    },
    /// Game code requested an engine view that its query did not declare.
    UndeclaredEngineView {
        /// Query whose declaration was checked.
        query: &'static str,
        /// Requested copied view.
        view: EngineViewKind,
    },
    /// A required copied engine view was absent from one matching row.
    MissingEngineView {
        /// Query containing the row.
        query: &'static str,
        /// Requested copied view.
        view: EngineViewKind,
        /// Row entity.
        entity: GameEntityHandle,
    },
    /// A copied engine view did not match its documented schema.
    InvalidEngineView {
        /// Copied view being decoded.
        view: EngineViewKind,
        /// Decode failure.
        reason: String,
    },
    /// A requested engine-owned global view was omitted or malformed.
    InvalidHostView {
        /// Global host view being decoded.
        view: GameHostViewKind,
        /// Decode failure.
        reason: String,
    },
    /// A host event payload did not match the selected typed stream.
    InvalidEvent {
        /// Host event stream.
        stream: GameEventStream,
        /// Monotonic event sequence.
        sequence: u64,
        /// Decode failure.
        reason: String,
    },
    /// A generated typed system returned an application error.
    System(String),
    /// The generated adapter violated an engine-owned lifecycle invariant.
    Internal(&'static str),
}

impl fmt::Display for GameApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingInputAction(name) => {
                write!(formatter, "declared input action `{name}` is missing")
            }
            Self::InvalidSaveValue { key, reason } => {
                write!(formatter, "save value `{key}` is invalid: {reason}")
            }
            Self::MissingResource(id) => {
                write!(formatter, "declared game resource `{id}` is missing")
            }
            Self::InvalidResource { id, reason } => {
                write!(formatter, "game resource `{id}` is invalid: {reason}")
            }
            Self::MissingQuery(id) => write!(formatter, "declared game query `{id}` is missing"),
            Self::UndeclaredComponent { query, component } => write!(
                formatter,
                "query `{query}` did not declare component `{component}`"
            ),
            Self::ReadOnlyComponent { query, component } => write!(
                formatter,
                "query `{query}` did not declare component `{component}` writable"
            ),
            Self::MissingComponent {
                query,
                component,
                entity,
            } => write!(
                formatter,
                "query `{query}` entity {}:{} is missing required component `{component}`",
                entity.id, entity.generation
            ),
            Self::InvalidComponent {
                query,
                component,
                entity,
                reason,
            } => write!(
                formatter,
                "query `{query}` entity {}:{} has invalid component `{component}`: {reason}",
                entity.id, entity.generation
            ),
            Self::UndeclaredEngineView { query, view } => {
                write!(
                    formatter,
                    "query `{query}` did not declare engine view `{view:?}`"
                )
            }
            Self::MissingEngineView {
                query,
                view,
                entity,
            } => write!(
                formatter,
                "query `{query}` entity {}:{} is missing required engine view `{view:?}`",
                entity.id, entity.generation
            ),
            Self::InvalidEngineView { view, reason } => {
                write!(formatter, "engine view `{view:?}` is invalid: {reason}")
            }
            Self::InvalidHostView { view, reason } => {
                write!(formatter, "host view `{view:?}` is invalid: {reason}")
            }
            Self::InvalidEvent {
                stream,
                sequence,
                reason,
            } => write!(
                formatter,
                "event `{stream:?}` sequence {sequence} is invalid: {reason}"
            ),
            Self::System(reason) => write!(formatter, "game system failed: {reason}"),
            Self::Internal(reason) => {
                write!(formatter, "typed game API invariant failed: {reason}")
            }
        }
    }
}

impl std::error::Error for GameApiError {}

/// A value that declares and fetches one typed game-system parameter.
///
/// Implementations are engine-owned. The public trait is required so generated
/// code in a project crate can derive access metadata from the exact function
/// parameter types.
pub trait GameSystemParam: sealed::SystemParam + Sized {
    /// Adds this parameter's complete host access to a system declaration.
    #[doc(hidden)]
    fn declare(access: &mut GameSystemAccess);

    /// Builds the parameter from one validated host invocation.
    #[doc(hidden)]
    fn fetch(input: &GameInvocation, output: TypedOutput) -> Result<Self, GameApiError>;
}

/// Frame and fixed-step timing supplied to a typed game system.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Time(GameClock);

impl Time {
    /// Returns rendered-frame delta time in seconds.
    pub fn delta_seconds(self) -> f32 {
        self.0.delta_seconds
    }

    /// Returns fixed simulation delta time in seconds.
    pub fn fixed_delta_seconds(self) -> f32 {
        self.0.fixed_delta_seconds
    }

    /// Returns seconds elapsed since the runtime started.
    pub fn elapsed_seconds(self) -> f64 {
        self.0.elapsed_seconds
    }

    /// Returns the rendered-frame index.
    pub fn frame_index(self) -> u64 {
        self.0.frame_index
    }

    /// Returns the fixed-step index.
    pub fn fixed_step_index(self) -> u64 {
        self.0.fixed_step_index
    }
}

impl GameSystemParam for Time {
    fn declare(_: &mut GameSystemAccess) {}

    fn fetch(input: &GameInvocation, _: TypedOutput) -> Result<Self, GameApiError> {
        Ok(Self(input.clock))
    }
}
impl sealed::SystemParam for Time {}

/// Stable input action marker implemented by project-owned zero-sized types.
pub trait InputAction {
    /// Project Settings action name.
    const NAME: &'static str;
}

/// Typed state for one declared Project Settings input action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Action<A: InputAction> {
    state: crate::game_io::GameInputActionState,
    marker: PhantomData<fn() -> A>,
}

impl<A: InputAction> Action<A> {
    /// Returns whether the action is currently active.
    pub fn pressed(self) -> bool {
        self.state.pressed
    }

    /// Returns whether the action became active in this input frame.
    pub fn just_pressed(self) -> bool {
        self.state.just_pressed
    }

    /// Returns whether the action became inactive in this input frame.
    pub fn just_released(self) -> bool {
        self.state.just_released
    }

    /// Returns the resolved scalar value.
    pub fn scalar(self) -> f32 {
        self.state.scalar
    }

    /// Returns the resolved two-dimensional value.
    pub fn vector(self) -> Vec2 {
        Vec2::from_array(self.state.vector)
    }
}

impl<A: InputAction> GameSystemParam for Action<A> {
    fn declare(access: &mut GameSystemAccess) {
        access.input_actions.push(A::NAME.to_owned());
    }

    fn fetch(input: &GameInvocation, _: TypedOutput) -> Result<Self, GameApiError> {
        input
            .input_actions
            .get(A::NAME)
            .copied()
            .map(|state| Self {
                state,
                marker: PhantomData,
            })
            .ok_or(GameApiError::MissingInputAction(A::NAME))
    }
}
impl<A: InputAction> sealed::SystemParam for Action<A> {}

/// Stable save key and Rust value type declared by a project marker.
pub trait SaveKey {
    /// Rust value stored at this key.
    type Value: GameField;
    /// Key in the active save document.
    const NAME: &'static str;
}

/// Optional typed value for one explicitly declared active-save key.
#[derive(Debug, Clone)]
pub struct SaveValue<K: SaveKey> {
    value: Option<K::Value>,
    marker: PhantomData<fn() -> K>,
}

impl<K: SaveKey> SaveValue<K> {
    /// Returns the decoded value when the active save contains the key.
    pub fn value(&self) -> Option<&K::Value> {
        self.value.as_ref()
    }

    /// Consumes the wrapper and returns the decoded optional value.
    pub fn into_value(self) -> Option<K::Value> {
        self.value
    }
}

impl<K: SaveKey> GameSystemParam for SaveValue<K> {
    fn declare(access: &mut GameSystemAccess) {
        access.save_keys.push(K::NAME.to_owned());
    }

    fn fetch(input: &GameInvocation, _: TypedOutput) -> Result<Self, GameApiError> {
        let value = input
            .save_values
            .get(K::NAME)
            .map(K::Value::from_value)
            .transpose()
            .map_err(|reason| GameApiError::InvalidSaveValue {
                key: K::NAME,
                reason,
            })?;
        Ok(Self {
            value,
            marker: PhantomData,
        })
    }
}
impl<K: SaveKey> sealed::SystemParam for SaveValue<K> {}

/// Read-only typed project resource.
#[derive(Debug, Clone)]
pub struct Res<T: GameResource>(T);

impl<T: GameResource> Deref for Res<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: GameResource> GameSystemParam for Res<T> {
    fn declare(access: &mut GameSystemAccess) {
        access.resources.push(GameResourceAccess {
            id: T::RESOURCE_ID.to_owned(),
            mode: GameAccessMode::Read,
        });
    }

    fn fetch(input: &GameInvocation, _: TypedOutput) -> Result<Self, GameApiError> {
        let value = input
            .resources
            .get(T::RESOURCE_ID)
            .ok_or(GameApiError::MissingResource(T::RESOURCE_ID))?;
        T::from_value(value)
            .map(Self)
            .map_err(|reason| GameApiError::InvalidResource {
                id: T::RESOURCE_ID,
                reason,
            })
    }
}
impl<T: GameResource> sealed::SystemParam for Res<T> {}

/// Writable typed project resource.
///
/// The complete validated value is patched back when the parameter is dropped,
/// including early returns from the game system.
pub struct ResMut<T: GameResource> {
    value: Option<T>,
    output: TypedOutput,
}

impl<T: GameResource> Deref for ResMut<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value
            .as_ref()
            .expect("resource value exists until its typed wrapper is dropped")
    }
}

impl<T: GameResource> DerefMut for ResMut<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value
            .as_mut()
            .expect("resource value exists until its typed wrapper is dropped")
    }
}

impl<T: GameResource> Drop for ResMut<T> {
    fn drop(&mut self) {
        let Some(value) = self.value.take() else {
            return;
        };
        self.output.with_mut(|output| {
            output.resource_patches.push(GameResourcePatch {
                resource_id: T::RESOURCE_ID.to_owned(),
                value: value.to_value(),
            });
        });
    }
}

impl<T: GameResource> GameSystemParam for ResMut<T> {
    fn declare(access: &mut GameSystemAccess) {
        access.resources.push(GameResourceAccess {
            id: T::RESOURCE_ID.to_owned(),
            mode: GameAccessMode::Write,
        });
    }

    fn fetch(input: &GameInvocation, output: TypedOutput) -> Result<Self, GameApiError> {
        let value = input
            .resources
            .get(T::RESOURCE_ID)
            .ok_or(GameApiError::MissingResource(T::RESOURCE_ID))?;
        let value = T::from_value(value).map_err(|reason| GameApiError::InvalidResource {
            id: T::RESOURCE_ID,
            reason,
        })?;
        Ok(Self {
            value: Some(value),
            output,
        })
    }
}
impl<T: GameResource> sealed::SystemParam for ResMut<T> {}

/// Type-safe declaration for one project entity query.
pub trait QuerySpec {
    /// Stable dotted query ID used by access metadata and diagnostics.
    const ID: &'static str;
    /// Stable query ID and component/view requirements.
    fn access() -> GameQueryAccess;
}

/// Fluent builder used by project query specifications.
pub struct QueryAccessBuilder {
    access: GameQueryAccess,
}

impl QueryAccessBuilder {
    /// Starts a query declaration with a stable dotted ID.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            access: GameQueryAccess {
                id: id.into(),
                components: Vec::new(),
                engine_views: Vec::new(),
            },
        }
    }

    /// Requires a read-only project component on every matching entity.
    pub fn read<T: GameComponent>(mut self) -> Self {
        self.access
            .components
            .push(component_access::<T>(GameAccessMode::Read, true));
        self
    }

    /// Copies a read-only project component when present without filtering rows.
    pub fn optional<T: GameComponent>(mut self) -> Self {
        self.access
            .components
            .push(component_access::<T>(GameAccessMode::Read, false));
        self
    }

    /// Requires a writable project component on every matching entity.
    pub fn write<T: GameComponent>(mut self) -> Self {
        self.access
            .components
            .push(component_access::<T>(GameAccessMode::Write, true));
        self
    }

    /// Copies a writable project component when present without filtering rows.
    pub fn optional_write<T: GameComponent>(mut self) -> Self {
        self.access
            .components
            .push(component_access::<T>(GameAccessMode::Write, false));
        self
    }

    /// Requires one typed engine view on every matching entity.
    pub fn view<V: EngineView>(mut self) -> Self {
        self.access.engine_views.push(GameEngineViewAccess {
            view: V::KIND,
            required: true,
        });
        self
    }

    /// Copies one typed engine view when present without filtering rows.
    pub fn optional_view<V: EngineView>(mut self) -> Self {
        self.access.engine_views.push(GameEngineViewAccess {
            view: V::KIND,
            required: false,
        });
        self
    }

    /// Completes the query declaration.
    pub fn build(self) -> GameQueryAccess {
        self.access
    }
}

fn component_access<T: GameComponent>(mode: GameAccessMode, required: bool) -> GameComponentAccess {
    GameComponentAccess {
        component_type: ComponentTypeId::new(T::TYPE_ID),
        mode,
        required,
    }
}

/// One row returned by a typed project query.
pub struct QueryRow<S: QuerySpec> {
    raw: GameQueryRow,
    access: GameQueryAccess,
    marker: PhantomData<fn() -> S>,
}

impl<S: QuerySpec> QueryRow<S> {
    /// Returns the generation-checked runtime entity handle.
    pub fn entity(&self) -> GameEntityHandle {
        self.raw.entity
    }

    /// Returns the stable authoring identity when this row originated in a scene or prefab.
    pub fn authoring_id(&self) -> Option<&EntityId> {
        self.raw.authoring_id.as_ref()
    }

    /// Decodes a declared project component as its Rust type.
    pub fn component<T: GameComponent>(&self) -> Result<T, GameApiError> {
        let component_type = ComponentTypeId::new(T::TYPE_ID);
        self.declared_component(&component_type, T::TYPE_ID)?;
        let value =
            self.raw
                .components
                .get(&component_type)
                .ok_or(GameApiError::MissingComponent {
                    query: query_id::<S>(&self.access),
                    component: T::TYPE_ID,
                    entity: self.raw.entity,
                })?;
        T::from_authoring_value(value).map_err(|reason| GameApiError::InvalidComponent {
            query: query_id::<S>(&self.access),
            component: T::TYPE_ID,
            entity: self.raw.entity,
            reason,
        })
    }

    /// Decodes a declared optional project component when it exists on this row.
    pub fn optional_component<T: GameComponent>(&self) -> Result<Option<T>, GameApiError> {
        let component_type = ComponentTypeId::new(T::TYPE_ID);
        self.declared_component(&component_type, T::TYPE_ID)?;
        self.raw
            .components
            .get(&component_type)
            .map(T::from_authoring_value)
            .transpose()
            .map_err(|reason| GameApiError::InvalidComponent {
                query: query_id::<S>(&self.access),
                component: T::TYPE_ID,
                entity: self.raw.entity,
                reason,
            })
    }

    /// Decodes a declared required engine view.
    pub fn view<V: EngineView>(&self) -> Result<V, GameApiError> {
        self.declared_view(V::KIND)?;
        let value = self
            .raw
            .engine_views
            .get(&V::KIND)
            .ok_or(GameApiError::MissingEngineView {
                query: query_id::<S>(&self.access),
                view: V::KIND,
                entity: self.raw.entity,
            })?;
        V::decode(value).map_err(|reason| GameApiError::InvalidEngineView {
            view: V::KIND,
            reason,
        })
    }

    /// Decodes a declared optional engine view when it exists on this row.
    pub fn optional_view<V: EngineView>(&self) -> Result<Option<V>, GameApiError> {
        self.declared_view(V::KIND)?;
        self.raw
            .engine_views
            .get(&V::KIND)
            .map(V::decode)
            .transpose()
            .map_err(|reason| GameApiError::InvalidEngineView {
                view: V::KIND,
                reason,
            })
    }

    fn declared_component(
        &self,
        component_type: &ComponentTypeId,
        component_name: &'static str,
    ) -> Result<&GameComponentAccess, GameApiError> {
        self.access
            .components
            .iter()
            .find(|access| &access.component_type == component_type)
            .ok_or(GameApiError::UndeclaredComponent {
                query: query_id::<S>(&self.access),
                component: component_name,
            })
    }

    fn declared_view(&self, view: EngineViewKind) -> Result<(), GameApiError> {
        self.access
            .engine_views
            .iter()
            .any(|access| access.view == view)
            .then_some(())
            .ok_or(GameApiError::UndeclaredEngineView {
                query: query_id::<S>(&self.access),
                view,
            })
    }
}

fn query_id<S: QuerySpec>(access: &GameQueryAccess) -> &'static str {
    let _ = access;
    S::ID
}

/// Typed rows and validated writable component patches for one query spec.
pub struct Query<S: QuerySpec> {
    access: GameQueryAccess,
    rows: Vec<QueryRow<S>>,
    output: TypedOutput,
}

impl<S: QuerySpec> Query<S> {
    /// Returns all matching rows in deterministic runtime iteration order.
    pub fn rows(&self) -> &[QueryRow<S>] {
        &self.rows
    }

    /// Returns an iterator over all matching rows.
    pub fn iter(&self) -> impl Iterator<Item = &QueryRow<S>> {
        self.rows.iter()
    }

    /// Replaces one declared writable component after validating its Rust type.
    pub fn set<T: GameComponent>(
        &mut self,
        entity: GameEntityHandle,
        component: T,
    ) -> Result<(), GameApiError> {
        let component_type = ComponentTypeId::new(T::TYPE_ID);
        let declaration = self
            .access
            .components
            .iter()
            .find(|access| access.component_type == component_type)
            .ok_or(GameApiError::UndeclaredComponent {
                query: query_id::<S>(&self.access),
                component: T::TYPE_ID,
            })?;
        if declaration.mode != GameAccessMode::Write {
            return Err(GameApiError::ReadOnlyComponent {
                query: query_id::<S>(&self.access),
                component: T::TYPE_ID,
            });
        }
        let row = self
            .rows
            .iter()
            .find(|row| row.raw.entity == entity)
            .ok_or(GameApiError::MissingComponent {
                query: query_id::<S>(&self.access),
                component: T::TYPE_ID,
                entity,
            })?;
        if !row.raw.components.contains_key(&component_type) {
            return Err(GameApiError::MissingComponent {
                query: query_id::<S>(&self.access),
                component: T::TYPE_ID,
                entity,
            });
        }
        self.output.with_mut(|output| {
            output
                .component_patches
                .push(crate::game_io::GameComponentPatch {
                    entity,
                    component_type,
                    value: component.to_authoring_value(),
                });
        });
        Ok(())
    }
}

impl<S: QuerySpec> GameSystemParam for Query<S> {
    fn declare(access: &mut GameSystemAccess) {
        access.queries.push(S::access());
    }

    fn fetch(input: &GameInvocation, output: TypedOutput) -> Result<Self, GameApiError> {
        let access = S::access();
        let result = input
            .queries
            .iter()
            .find(|query| query.query_id == access.id)
            .ok_or_else(|| GameApiError::MissingQuery(query_id::<S>(&access)))?;
        let rows = result
            .rows
            .iter()
            .cloned()
            .map(|raw| QueryRow {
                raw,
                access: access.clone(),
                marker: PhantomData,
            })
            .collect();
        Ok(Self {
            access,
            rows,
            output,
        })
    }
}
impl<S: QuerySpec> sealed::SystemParam for Query<S> {}

/// Typed decoder contract for an engine-owned copied entity view.
pub trait EngineView: Sized {
    /// Stable ABI view kind declared by a query.
    const KIND: EngineViewKind;
    /// Decodes the host copy into a public Rust value.
    #[doc(hidden)]
    fn decode(value: &Value) -> Result<Self, String>;
}

/// Local or global transform copied from the runtime.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformView {
    /// Translation in local or world space according to the requested view.
    pub translation: Vec3,
    /// Rotation in local or world space.
    pub rotation: Quat,
    /// Scale in local or world space.
    pub scale: Vec3,
}

/// Local transform view marker and decoded value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalTransformView(pub TransformView);

/// Global transform view marker and decoded value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlobalTransformView(pub TransformView);

impl Deref for LocalTransformView {
    type Target = TransformView;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Deref for GlobalTransformView {
    type Target = TransformView;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl EngineView for LocalTransformView {
    const KIND: EngineViewKind = EngineViewKind::Transform;
    fn decode(value: &Value) -> Result<Self, String> {
        decode_transform(value).map(Self)
    }
}

impl EngineView for GlobalTransformView {
    const KIND: EngineViewKind = EngineViewKind::GlobalTransform;
    fn decode(value: &Value) -> Result<Self, String> {
        decode_transform(value).map(Self)
    }
}

/// Stable authoring metadata copied for one runtime entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringIdentityView {
    /// Stable persisted entity ID.
    pub id: EntityId,
    /// Current authoring name.
    pub name: String,
    /// Search and gameplay tags.
    pub tags: Vec<String>,
    /// Optional project-authored team label.
    pub team: String,
}

impl EngineView for AuthoringIdentityView {
    const KIND: EngineViewKind = EngineViewKind::AuthoringIdentity;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        Ok(Self {
            id: EntityId::from_stable_id(StableId::new(string_field(fields, "id")?))
                .map_err(|error| error.to_string())?,
            name: string_field(fields, "name")?.to_owned(),
            tags: string_array(field(fields, "tags")?)?,
            team: string_field(fields, "team")?.to_owned(),
        })
    }
}

/// Kinematic character state copied for one entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterStateView {
    /// Current world velocity.
    pub velocity: Vec3,
    /// Whether the controller is grounded.
    pub grounded: bool,
    /// Current world-facing direction when a transform is present.
    pub facing: Option<Vec3>,
    /// Gravity multiplier used by the controller.
    pub gravity_scale: f32,
}

impl EngineView for CharacterStateView {
    const KIND: EngineViewKind = EngineViewKind::CharacterState;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        Ok(Self {
            velocity: vec3(field(fields, "velocity")?)?,
            grounded: bool_field(fields, "grounded")?,
            facing: optional_vec3(field(fields, "facing")?)?,
            gravity_scale: number_field(fields, "gravity_scale")? as f32,
        })
    }
}

/// Playback mode of a copied animator state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationPlaybackState {
    /// Playback is stopped and reset.
    Stopped,
    /// Playback is advancing.
    Playing,
    /// Playback retains its time without advancing.
    Paused,
}

/// Animator state copied for one entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationStateView {
    /// Process-local clip identifier valid for animation commands in this run.
    pub clip_runtime_id: u64,
    /// Current playback mode.
    pub state: AnimationPlaybackState,
    /// Current clip time in seconds.
    pub time: f32,
    /// Whether playback loops.
    pub looping: bool,
    /// Whether a crossfade is active.
    pub fading: bool,
}

impl EngineView for AnimationStateView {
    const KIND: EngineViewKind = EngineViewKind::AnimationState;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        let state = match string_field(fields, "state")? {
            "stopped" => AnimationPlaybackState::Stopped,
            "playing" => AnimationPlaybackState::Playing,
            "paused" => AnimationPlaybackState::Paused,
            value => return Err(format!("unknown animation state `{value}`")),
        };
        Ok(Self {
            clip_runtime_id: unsigned_field(fields, "clip_runtime_id")?,
            state,
            time: number_field(fields, "time")? as f32,
            looping: bool_field(fields, "looping")?,
            fading: bool_field(fields, "fading")?,
        })
    }
}

/// Current lock-on selection copied for each queried entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockOnStateView {
    /// Current target selected by the global lock-on service.
    pub current: Option<GameEntityHandle>,
    /// Whether the queried row is the selected target.
    pub is_current_target: bool,
}

impl EngineView for LockOnStateView {
    const KIND: EngineViewKind = EngineViewKind::LockOnState;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        Ok(Self {
            current: optional_entity(field(fields, "current")?)?,
            is_current_target: bool_field(fields, "is_current_target")?,
        })
    }
}

/// Attack hitbox state copied for one carrier entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HitboxStateView {
    /// Entity credited as the hitbox owner.
    pub owner: GameEntityHandle,
    /// Gameplay team used by engine-owned hit filtering.
    pub team: i32,
    /// Damage applied on an accepted contact.
    pub damage: f32,
    /// Whether an activation may hit each target only once.
    pub one_hit_per_target: bool,
    /// Whether collision detection currently includes this hitbox.
    pub enabled: bool,
    /// Monotonic hitbox activation number.
    pub activation: u64,
    /// Number of targets recorded in the current activation.
    pub hit_count: usize,
}

impl EngineView for HitboxStateView {
    const KIND: EngineViewKind = EngineViewKind::HitboxState;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        Ok(Self {
            owner: entity(field(fields, "owner")?)?,
            team: signed_field(fields, "team")?
                .try_into()
                .map_err(|_| "team is outside i32".to_owned())?,
            damage: number_field(fields, "damage")? as f32,
            one_hit_per_target: bool_field(fields, "one_hit_per_target")?,
            enabled: bool_field(fields, "enabled")?,
            activation: unsigned_field(fields, "activation")?,
            hit_count: unsigned_field(fields, "hit_count")?
                .try_into()
                .map_err(|_| "hit count is outside usize".to_owned())?,
        })
    }
}

/// Engine-owned damage receiver state copied for one entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DamageReceiverView {
    /// Gameplay team used by engine-owned hit filtering.
    pub team: i32,
    /// Current hit points.
    pub health: f32,
    /// Maximum hit points.
    pub max_health: f32,
    /// Remaining invulnerability duration in seconds.
    pub invulnerability_remaining: f32,
}

impl EngineView for DamageReceiverView {
    const KIND: EngineViewKind = EngineViewKind::DamageReceiverState;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        Ok(Self {
            team: signed_field(fields, "team")?
                .try_into()
                .map_err(|_| "team is outside i32".to_owned())?,
            health: number_field(fields, "health")? as f32,
            max_health: number_field(fields, "max_health")? as f32,
            invulnerability_remaining: number_field(fields, "invulnerability_remaining")? as f32,
        })
    }
}

/// Current navigation path state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationStatus {
    /// No destination is assigned.
    Idle,
    /// No navigation mesh is available.
    MissingNavMesh,
    /// A destination exists but no path could be found.
    NoPath,
    /// The agent is following a path.
    Moving,
    /// The agent reached its stopping distance.
    Arrived,
}

/// Navigation agent state copied for one entity.
#[derive(Debug, Clone, PartialEq)]
pub struct NavigationStateView {
    /// Current world destination when assigned.
    pub target: Option<Vec3>,
    /// Desired movement speed.
    pub speed: f32,
    /// Arrival distance threshold.
    pub stopping_distance: f32,
    /// Whether the agent has no active path work.
    pub idle: bool,
    /// Explicit navigation result state.
    pub status: NavigationStatus,
    /// Seconds between automatic repath attempts.
    pub repath_interval: f32,
    /// Current world-space waypoints.
    pub path: Vec<Vec3>,
}

impl EngineView for NavigationStateView {
    const KIND: EngineViewKind = EngineViewKind::NavigationState;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        let status = match string_field(fields, "status")? {
            "idle" => NavigationStatus::Idle,
            "missing_navmesh" => NavigationStatus::MissingNavMesh,
            "no_path" => NavigationStatus::NoPath,
            "moving" => NavigationStatus::Moving,
            "arrived" => NavigationStatus::Arrived,
            value => return Err(format!("unknown navigation status `{value}`")),
        };
        let path = array(field(fields, "path")?)?
            .iter()
            .map(vec3)
            .collect::<Result<_, _>>()?;
        Ok(Self {
            target: optional_vec3(field(fields, "target")?)?,
            speed: number_field(fields, "speed")? as f32,
            stopping_distance: number_field(fields, "stopping_distance")? as f32,
            idle: bool_field(fields, "idle")?,
            status,
            repath_interval: number_field(fields, "repath_interval")? as f32,
            path,
        })
    }
}

/// Behavior Tree leaf kind visited in the latest tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorVisit {
    /// Whether the leaf was an action rather than a condition.
    pub is_action: bool,
    /// Stable project behavior identifier.
    pub behavior_id: String,
}

/// Latest Behavior Tree execution state copied for one entity.
#[derive(Debug, Clone, PartialEq)]
pub struct BehaviorTreeStateView {
    /// Whether the runner is enabled.
    pub enabled: bool,
    /// Last result when the tree has ticked.
    pub status: Option<GameBehaviorStatus>,
    /// Last dispatch error when execution failed.
    pub error: Option<String>,
    /// Leaves visited by the latest tick.
    pub visited: Vec<BehaviorVisit>,
    blackboard: BTreeMap<String, Value>,
}

impl BehaviorTreeStateView {
    /// Decodes a named blackboard value through a concrete [`GameField`] type.
    pub fn blackboard<T: GameField>(&self, name: &str) -> Result<Option<T>, String> {
        self.blackboard.get(name).map(T::from_value).transpose()
    }
}

impl EngineView for BehaviorTreeStateView {
    const KIND: EngineViewKind = EngineViewKind::BehaviorTreeState;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        let status = match optional_string(field(fields, "status")?)? {
            Some("success") => Some(GameBehaviorStatus::Success),
            Some("failure") => Some(GameBehaviorStatus::Failure),
            Some("running") => Some(GameBehaviorStatus::Running),
            Some(value) => return Err(format!("unknown behavior status `{value}`")),
            None => None,
        };
        let visited = array(field(fields, "visited")?)?
            .iter()
            .map(|value| {
                let fields = object(value)?;
                let is_action = match string_field(fields, "kind")? {
                    "action" => true,
                    "condition" => false,
                    value => return Err(format!("unknown behavior visit kind `{value}`")),
                };
                Ok(BehaviorVisit {
                    is_action,
                    behavior_id: string_field(fields, "behavior_id")?.to_owned(),
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            enabled: bool_field(fields, "enabled")?,
            status,
            error: optional_string(field(fields, "error")?)?.map(str::to_owned),
            visited,
            blackboard: object(field(fields, "blackboard")?)?.clone(),
        })
    }
}

/// Read-only snapshot of global runtime UI bindings.
#[derive(Debug, Clone, PartialEq)]
pub struct UiBindingsView(BTreeMap<String, Value>);

impl UiBindingsView {
    /// Returns a text binding and rejects a mismatched existing value.
    pub fn text(&self, name: &str) -> Result<Option<&str>, String> {
        self.0.get(name).map(string).transpose()
    }

    /// Returns a numeric binding and rejects a mismatched existing value.
    pub fn number(&self, name: &str) -> Result<Option<f64>, String> {
        self.0.get(name).map(number).transpose()
    }

    /// Returns a boolean binding and rejects a mismatched existing value.
    pub fn flag(&self, name: &str) -> Result<Option<bool>, String> {
        self.0.get(name).map(boolean).transpose()
    }
}

impl EngineView for UiBindingsView {
    const KIND: EngineViewKind = EngineViewKind::UiBindings;
    fn decode(value: &Value) -> Result<Self, String> {
        object(value).cloned().map(Self)
    }
}

/// Typed contract for an engine-owned global view.
pub trait HostView: Sized {
    /// Stable host view kind.
    const KIND: GameHostViewKind;
    /// Decodes the host copy.
    #[doc(hidden)]
    fn decode(value: &Value) -> Result<Self, String>;
}

/// One required engine-owned global view.
pub struct View<V: HostView>(V);

impl<V: HostView> Deref for View<V> {
    type Target = V;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<V: HostView> GameSystemParam for View<V> {
    fn declare(access: &mut GameSystemAccess) {
        access.host_views.push(V::KIND);
    }
    fn fetch(input: &GameInvocation, _: TypedOutput) -> Result<Self, GameApiError> {
        let value =
            input
                .host_views
                .get(&V::KIND)
                .ok_or_else(|| GameApiError::InvalidHostView {
                    view: V::KIND,
                    reason: "declared view is missing".to_owned(),
                })?;
        V::decode(value)
            .map(Self)
            .map_err(|reason| GameApiError::InvalidHostView {
                view: V::KIND,
                reason,
            })
    }
}
impl<V: HostView> sealed::SystemParam for View<V> {}

/// Current scene service state copied independently from transition events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneStateView {
    /// Current project-relative scene path.
    pub current_path: Option<String>,
    /// Scene path waiting for the next transition boundary.
    pub pending_path: Option<String>,
    /// Successful scene generation counter.
    pub generation: u64,
    /// Most recent failed scene path.
    pub failure_path: Option<String>,
    /// Most recent scene failure message.
    pub failure_message: Option<String>,
}

impl HostView for SceneStateView {
    const KIND: GameHostViewKind = GameHostViewKind::SceneState;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        Ok(Self {
            current_path: optional_string(field(fields, "current_path")?)?.map(str::to_owned),
            pending_path: optional_string(field(fields, "pending_path")?)?.map(str::to_owned),
            generation: unsigned_field(fields, "generation")?,
            failure_path: optional_string(field(fields, "failure_path")?)?.map(str::to_owned),
            failure_message: optional_string(field(fields, "failure_message")?)?.map(str::to_owned),
        })
    }
}

/// Typed contract for one engine-owned host event stream.
pub trait HostEvent: Sized {
    /// Stable stream copied into the invocation.
    const STREAM: GameEventStream;
    /// Decodes one stream payload.
    #[doc(hidden)]
    fn decode(value: &Value) -> Result<Self, String>;
}

/// Sequence-numbered typed host event.
#[derive(Debug, Clone, PartialEq)]
pub struct Event<E> {
    /// Monotonic stream-local sequence.
    pub sequence: u64,
    /// Decoded event payload.
    pub value: E,
}

/// Typed event reader that acknowledges every successfully decoded record.
pub struct Events<E: HostEvent> {
    events: Vec<Event<E>>,
}

impl<E: HostEvent> Events<E> {
    /// Returns decoded unconsumed events in host order.
    pub fn iter(&self) -> impl Iterator<Item = &Event<E>> {
        self.events.iter()
    }
    /// Returns whether no unconsumed events were supplied.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl<E: HostEvent> GameSystemParam for Events<E> {
    fn declare(access: &mut GameSystemAccess) {
        access.event_streams.push(E::STREAM);
    }
    fn fetch(input: &GameInvocation, output: TypedOutput) -> Result<Self, GameApiError> {
        let mut events = Vec::new();
        let mut consumed = None;
        for record in input
            .events
            .iter()
            .filter(|record| record.stream == E::STREAM)
        {
            let value =
                E::decode(&record.payload).map_err(|reason| GameApiError::InvalidEvent {
                    stream: E::STREAM,
                    sequence: record.sequence,
                    reason,
                })?;
            consumed =
                Some(consumed.map_or(record.sequence, |current: u64| current.max(record.sequence)));
            events.push(Event {
                sequence: record.sequence,
                value,
            });
        }
        if let Some(sequence) = consumed {
            output.with_mut(|output| {
                output.consumed_event_sequences.insert(E::STREAM, sequence);
            });
        }
        Ok(Self { events })
    }
}
impl<E: HostEvent> sealed::SystemParam for Events<E> {}

/// Collision transition phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionEventPhase {
    /// The pair began overlapping.
    Enter,
    /// The pair remained overlapping.
    Stay,
    /// The pair stopped overlapping.
    Exit,
}

/// Typed collision transition event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CollisionEvent {
    /// Transition phase.
    pub phase: CollisionEventPhase,
    /// First colliding entity.
    pub entity_a: GameEntityHandle,
    /// Second colliding entity.
    pub entity_b: GameEntityHandle,
    /// Engine-computed separation vector.
    pub push_out: Vec3,
    /// Whether either collider is a trigger.
    pub is_trigger: bool,
}

impl HostEvent for CollisionEvent {
    const STREAM: GameEventStream = GameEventStream::Collision;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        let phase = match string_field(fields, "phase")? {
            "enter" => CollisionEventPhase::Enter,
            "stay" => CollisionEventPhase::Stay,
            "exit" => CollisionEventPhase::Exit,
            value => return Err(format!("unknown collision phase `{value}`")),
        };
        Ok(Self {
            phase,
            entity_a: entity(field(fields, "entity_a")?)?,
            entity_b: entity(field(fields, "entity_b")?)?,
            push_out: vec3(field(fields, "push_out")?)?,
            is_trigger: bool_field(fields, "is_trigger")?,
        })
    }
}

/// Typed accepted-hit event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HitEvent {
    /// Entity credited with the hit.
    pub attacker: GameEntityHandle,
    /// Hitbox carrier entity.
    pub hitbox: GameEntityHandle,
    /// Damaged entity.
    pub target: GameEntityHandle,
    /// Applied damage.
    pub damage: f32,
    /// Target health after damage.
    pub remaining_health: f32,
    /// Hitbox activation that produced the result.
    pub activation: u64,
}

impl HostEvent for HitEvent {
    const STREAM: GameEventStream = GameEventStream::Hit;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        Ok(Self {
            attacker: entity(field(fields, "attacker")?)?,
            hitbox: entity(field(fields, "hitbox")?)?,
            target: entity(field(fields, "target")?)?,
            damage: number_field(fields, "damage")? as f32,
            remaining_health: number_field(fields, "remaining_health")? as f32,
            activation: unsigned_field(fields, "activation")?,
        })
    }
}

/// Typed animation marker event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationEvent {
    /// Entity whose clip crossed the marker.
    pub entity: GameEntityHandle,
    /// Author-authored marker name.
    pub name: String,
}

impl HostEvent for AnimationEvent {
    const STREAM: GameEventStream = GameEventStream::Animation;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        Ok(Self {
            entity: entity(field(fields, "entity")?)?,
            name: string_field(fields, "name")?.to_owned(),
        })
    }
}

/// Typed runtime UI interaction event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiEvent {
    /// Stable UI event name.
    pub name: String,
}

impl HostEvent for UiEvent {
    const STREAM: GameEventStream = GameEventStream::Ui;
    fn decode(value: &Value) -> Result<Self, String> {
        Ok(Self {
            name: string_field(object(value)?, "name")?.to_owned(),
        })
    }
}

/// Scene transition lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneEvent {
    /// A scene switch completed and advanced the generation.
    Completed {
        /// New current path when the scene manager reports one.
        path: Option<String>,
        /// New scene generation.
        generation: u64,
    },
    /// A scene switch failed without replacing the current scene.
    Failed {
        /// Requested project-relative scene path.
        path: String,
        /// Actionable loader failure.
        message: String,
    },
}

impl HostEvent for SceneEvent {
    const STREAM: GameEventStream = GameEventStream::Scene;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        match string_field(fields, "status")? {
            "completed" => Ok(Self::Completed {
                path: optional_string(field(fields, "path")?)?.map(str::to_owned),
                generation: unsigned_field(fields, "generation")?,
            }),
            "failed" => Ok(Self::Failed {
                path: string_field(fields, "path")?.to_owned(),
                message: string_field(fields, "message")?.to_owned(),
            }),
            status => Err(format!("unknown scene event status `{status}`")),
        }
    }
}

/// Gameplay timer state returned by completion and query events.
#[derive(Debug, Clone, PartialEq)]
pub enum TimerEvent {
    /// A timer reached zero.
    Completed {
        /// Stable timer ID.
        timer_id: String,
    },
    /// Result of an explicit timer query command.
    QueryResult {
        /// Stable timer ID.
        timer_id: String,
        /// Caller-provided request ID.
        request_id: u64,
        /// Active, completed, or missing state.
        status: String,
        /// Remaining seconds for active or completed timers.
        remaining_seconds: Option<f32>,
    },
}

impl HostEvent for TimerEvent {
    const STREAM: GameEventStream = GameEventStream::Timer;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        match string_field(fields, "kind")? {
            "completed" => Ok(Self::Completed {
                timer_id: string_field(fields, "timer_id")?.to_owned(),
            }),
            "query_result" => Ok(Self::QueryResult {
                timer_id: string_field(fields, "timer_id")?.to_owned(),
                request_id: unsigned_field(fields, "request_id")?,
                status: string_field(fields, "status")?.to_owned(),
                remaining_seconds: fields
                    .get("remaining_seconds")
                    .map(number)
                    .transpose()?
                    .map(|value| value as f32),
            }),
            kind => Err(format!("unknown timer event kind `{kind}`")),
        }
    }
}

/// Deferred prefab spawn result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnResultEvent {
    /// Caller-provided request ID.
    pub request_id: u64,
    /// Requested project-relative prefab path.
    pub path: String,
    /// Spawned runtime root on success.
    pub root: Option<GameEntityHandle>,
    /// Host failure message on failure.
    pub error: Option<String>,
}

impl HostEvent for SpawnResultEvent {
    const STREAM: GameEventStream = GameEventStream::SpawnResult;
    fn decode(value: &Value) -> Result<Self, String> {
        let fields = object(value)?;
        let status = string_field(fields, "status")?;
        let (root, error) = match status {
            "completed" => (Some(entity(field(fields, "root")?)?), None),
            "failed" => (None, Some(string_field(fields, "message")?.to_owned())),
            value => return Err(format!("unknown spawn result status `{value}`")),
        };
        Ok(Self {
            request_id: unsigned_field(fields, "request_id")?,
            path: string_field(fields, "path")?.to_owned(),
            root,
            error,
        })
    }
}

/// Marker for one project-defined typed event.
pub trait ProjectEvent {
    /// Payload encoded through the engine's supported field conversion.
    type Payload: GameField;
    /// Stable dotted event ID.
    const ID: &'static str;
}

/// Target and payload of one matching project-defined event.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectEventRecord<T> {
    /// Optional generation-checked target.
    pub target: Option<GameEntityHandle>,
    /// Decoded project payload.
    pub payload: T,
}

/// Typed reader for one project-defined event ID.
pub struct ProjectEvents<E: ProjectEvent> {
    events: Vec<Event<ProjectEventRecord<E::Payload>>>,
    marker: PhantomData<fn() -> E>,
}

impl<E: ProjectEvent> ProjectEvents<E> {
    /// Returns matching events in host sequence order.
    pub fn iter(&self) -> impl Iterator<Item = &Event<ProjectEventRecord<E::Payload>>> {
        self.events.iter()
    }
}

impl<E: ProjectEvent> GameSystemParam for ProjectEvents<E> {
    fn declare(access: &mut GameSystemAccess) {
        access.event_streams.push(GameEventStream::Game);
    }

    fn fetch(input: &GameInvocation, output: TypedOutput) -> Result<Self, GameApiError> {
        let mut events = Vec::new();
        let mut consumed = None;
        for record in input
            .events
            .iter()
            .filter(|record| record.stream == GameEventStream::Game)
        {
            let fields = object(&record.payload).map_err(|reason| GameApiError::InvalidEvent {
                stream: GameEventStream::Game,
                sequence: record.sequence,
                reason,
            })?;
            if string_field(fields, "event_id").map_err(|reason| GameApiError::InvalidEvent {
                stream: GameEventStream::Game,
                sequence: record.sequence,
                reason,
            })? == E::ID
            {
                let target = optional_entity(field(fields, "target").map_err(|reason| {
                    GameApiError::InvalidEvent {
                        stream: GameEventStream::Game,
                        sequence: record.sequence,
                        reason,
                    }
                })?)
                .map_err(|reason| GameApiError::InvalidEvent {
                    stream: GameEventStream::Game,
                    sequence: record.sequence,
                    reason,
                })?;
                let payload =
                    E::Payload::from_value(field(fields, "payload").map_err(|reason| {
                        GameApiError::InvalidEvent {
                            stream: GameEventStream::Game,
                            sequence: record.sequence,
                            reason,
                        }
                    })?)
                    .map_err(|reason| GameApiError::InvalidEvent {
                        stream: GameEventStream::Game,
                        sequence: record.sequence,
                        reason,
                    })?;
                events.push(Event {
                    sequence: record.sequence,
                    value: ProjectEventRecord { target, payload },
                });
            }
            consumed =
                Some(consumed.map_or(record.sequence, |value: u64| value.max(record.sequence)));
        }
        if let Some(sequence) = consumed {
            output.with_mut(|output| {
                output
                    .consumed_event_sequences
                    .insert(GameEventStream::Game, sequence);
            });
        }
        Ok(Self {
            events,
            marker: PhantomData,
        })
    }
}
impl<E: ProjectEvent> sealed::SystemParam for ProjectEvents<E> {}

/// Deferred host commands emitted through methods that never expose payload maps.
#[derive(Clone)]
pub struct Commands {
    output: TypedOutput,
}

impl Commands {
    pub(crate) fn push(&mut self, command: GameCommand) {
        self.output.with_mut(|output| output.commands.push(command));
    }
    /// Replaces a target's complete local transform.
    pub fn set_transform(
        &mut self,
        target: GameEntityHandle,
        translation: Vec3,
        rotation: Quat,
        scale: Vec3,
    ) {
        self.push(GameCommand::set_transform(
            target,
            translation.to_array(),
            rotation.to_array(),
            scale.to_array(),
        ));
    }
    /// Adds a local-space translation delta.
    pub fn translate(&mut self, target: GameEntityHandle, delta: Vec3) {
        self.push(GameCommand::translate(target, delta.to_array()));
    }
    /// Applies a normalized quaternion delta.
    pub fn rotate(&mut self, target: GameEntityHandle, delta: Quat) {
        self.push(GameCommand::rotate(target, delta.to_array()));
    }
    /// Removes a runtime entity at the schedule boundary.
    pub fn despawn(&mut self, target: GameEntityHandle) {
        self.push(GameCommand::despawn(target));
    }
    /// Adds a project component using its registered default.
    pub fn add_component<T: GameComponent>(&mut self, target: GameEntityHandle) {
        self.push(GameCommand::add_game_component(
            target,
            ComponentTypeId::new(T::TYPE_ID),
        ));
    }
    /// Removes a project component.
    pub fn remove_component<T: GameComponent>(&mut self, target: GameEntityHandle) {
        self.push(GameCommand::remove_game_component(
            target,
            ComponentTypeId::new(T::TYPE_ID),
        ));
    }
    /// Enables a retained project component.
    pub fn enable_component<T: GameComponent>(&mut self, target: GameEntityHandle) {
        self.push(GameCommand::enable_game_component(
            target,
            ComponentTypeId::new(T::TYPE_ID),
        ));
    }
    /// Disables a retained project component.
    pub fn disable_component<T: GameComponent>(&mut self, target: GameEntityHandle) {
        self.push(GameCommand::disable_game_component(
            target,
            ComponentTypeId::new(T::TYPE_ID),
        ));
    }
    /// Sets character-controller velocity and facing.
    pub fn set_character_motion(&mut self, target: GameEntityHandle, velocity: Vec3, facing: Vec3) {
        self.push(GameCommand::set_character_motion(
            target,
            velocity.to_array(),
            facing.to_array(),
        ));
    }
    /// Assigns a navigation destination.
    pub fn set_navigation_target(&mut self, target: GameEntityHandle, destination: Vec3) {
        self.push(GameCommand::set_navigation_target(
            target,
            destination.to_array(),
        ));
    }
    /// Clears a navigation destination.
    pub fn clear_navigation_target(&mut self, target: GameEntityHandle) {
        self.push(GameCommand::clear_navigation_target(target));
    }
    /// Requests a prefab spawn and later typed result event.
    pub fn spawn_prefab(&mut self, path: impl Into<String>, position: Vec3, request_id: u64) {
        self.push(GameCommand::spawn_prefab(
            path,
            position.to_array(),
            request_id,
        ));
    }
    /// Selects the nearest lock-on target.
    pub fn acquire_lock_on(&mut self) {
        self.push(GameCommand::acquire_lock_on());
    }
    /// Cycles the current lock-on target.
    pub fn cycle_lock_on(&mut self) {
        self.push(GameCommand::cycle_lock_on());
    }
    /// Releases lock-on.
    pub fn release_lock_on(&mut self) {
        self.push(GameCommand::release_lock_on());
    }
    /// Starts or resumes animation playback.
    pub fn play_animation(&mut self, target: GameEntityHandle, looping: bool) {
        self.push(GameCommand::play_animation(target, looping));
    }
    /// Crossfades to another runtime clip.
    pub fn crossfade_animation(
        &mut self,
        target: GameEntityHandle,
        clip_runtime_id: u64,
        duration_seconds: f32,
        looping: bool,
    ) {
        self.push(GameCommand::crossfade_animation(
            target,
            clip_runtime_id,
            duration_seconds,
            looping,
        ));
    }
    /// Stops animation playback.
    pub fn stop_animation(&mut self, target: GameEntityHandle) {
        self.push(GameCommand::stop_animation(target));
    }
    /// Creates a command-owned hitbox.
    #[allow(clippy::too_many_arguments)]
    pub fn create_hitbox(
        &mut self,
        target: GameEntityHandle,
        owner: GameEntityHandle,
        shape: GameHitboxShape,
        team: i32,
        damage: f32,
        membership: u32,
        mask: u32,
        one_hit_per_target: bool,
    ) {
        self.push(GameCommand::create_hitbox(
            target,
            owner,
            shape,
            team,
            damage,
            membership,
            mask,
            one_hit_per_target,
        ));
    }
    /// Creates a command-owned hitbox with knockback.
    #[allow(clippy::too_many_arguments)]
    pub fn create_hitbox_with_knockback(
        &mut self,
        target: GameEntityHandle,
        owner: GameEntityHandle,
        shape: GameHitboxShape,
        team: i32,
        damage: f32,
        membership: u32,
        mask: u32,
        one_hit_per_target: bool,
        knockback: Vec3,
    ) {
        self.push(GameCommand::create_hitbox_with_knockback(
            target,
            owner,
            shape,
            team,
            damage,
            membership,
            mask,
            one_hit_per_target,
            knockback.to_array(),
        ));
    }
    /// Enables a command-owned hitbox.
    pub fn enable_hitbox(&mut self, target: GameEntityHandle) {
        self.push(GameCommand::enable_hitbox(target));
    }
    /// Disables a command-owned hitbox.
    pub fn disable_hitbox(&mut self, target: GameEntityHandle) {
        self.push(GameCommand::disable_hitbox(target));
    }
    /// Removes a command-owned hitbox.
    pub fn remove_hitbox(&mut self, target: GameEntityHandle) {
        self.push(GameCommand::remove_hitbox(target));
    }
    /// Publishes a UI text binding.
    pub fn set_ui_text(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.push(GameCommand::set_ui_text(name, value));
    }
    /// Publishes a UI numeric binding.
    pub fn set_ui_number(&mut self, name: impl Into<String>, value: f64) {
        self.push(GameCommand::set_ui_number(name, value));
    }
    /// Publishes a UI boolean binding.
    pub fn set_ui_flag(&mut self, name: impl Into<String>, value: bool) {
        self.push(GameCommand::set_ui_flag(name, value));
    }
    /// Removes a UI binding.
    pub fn remove_ui_binding(&mut self, name: impl Into<String>) {
        self.push(GameCommand::remove_ui_binding(name));
    }
    /// Shows or hides an authored UI document.
    pub fn set_ui_document_visible(&mut self, target: GameEntityHandle, visible: bool) {
        self.push(GameCommand::set_ui_document_visible(target, visible));
    }
    /// Requests a scene transition.
    pub fn request_scene(&mut self, path: impl Into<String>) {
        self.push(GameCommand::request_scene(path));
    }
    /// Queues a sound effect.
    pub fn play_sound_effect(&mut self, asset_id: impl Into<String>) {
        self.push(GameCommand::play_sound_effect(asset_id));
    }
    /// Queues a sound effect whose position follows a generation-checked entity.
    pub fn play_spatial_sound_effect(
        &mut self,
        target: GameEntityHandle,
        asset_id: impl Into<String>,
        options: GameSpatialAudioOptions,
    ) {
        self.push(GameCommand::play_spatial_sound_effect(target, asset_id, options));
    }
    /// Replaces background music.
    pub fn play_background_music(&mut self, asset_id: impl Into<String>) {
        self.push(GameCommand::play_background_music(asset_id));
    }
    /// Crossfades background music.
    pub fn crossfade_background_music(&mut self, asset_id: impl Into<String>, fade_seconds: f32) {
        self.push(GameCommand::crossfade_background_music(
            asset_id,
            fade_seconds,
        ));
    }
    /// Stops background music.
    pub fn stop_background_music(&mut self) {
        self.push(GameCommand::stop_background_music());
    }
    /// Sets master mixer volume.
    pub fn set_master_volume(&mut self, volume: f32) {
        self.push(GameCommand::set_master_volume(volume));
    }
    /// Sets background-music volume.
    pub fn set_background_music_volume(&mut self, volume: f32) {
        self.push(GameCommand::set_background_music_volume(volume));
    }
    /// Sets sound-effect volume.
    pub fn set_sound_effect_volume(&mut self, volume: f32) {
        self.push(GameCommand::set_sound_effect_volume(volume));
    }
    /// Sets an active-save text value.
    pub fn set_save_text(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.push(GameCommand::set_save_text(key, value));
    }
    /// Sets an active-save number value.
    pub fn set_save_number(&mut self, key: impl Into<String>, value: f64) {
        self.push(GameCommand::set_save_number(key, value));
    }
    /// Sets an active-save boolean value.
    pub fn set_save_flag(&mut self, key: impl Into<String>, value: bool) {
        self.push(GameCommand::set_save_flag(key, value));
    }
    /// Removes an active-save value.
    pub fn remove_save_value(&mut self, key: impl Into<String>) {
        self.push(GameCommand::remove_save_value(key));
    }
    /// Writes the active save to a slot.
    pub fn write_save_slot(&mut self, slot: u32) {
        self.push(GameCommand::write_save_slot(slot));
    }
    /// Loads a save slot at the save-service boundary.
    pub fn load_save_slot(&mut self, slot: u32) {
        self.push(GameCommand::load_save_slot(slot));
    }
    /// Starts or replaces a gameplay timer.
    pub fn set_timer(&mut self, id: impl Into<String>, duration_seconds: f32) {
        self.push(GameCommand::set_timer(id, duration_seconds));
    }
    /// Cancels a gameplay timer.
    pub fn cancel_timer(&mut self, id: impl Into<String>) {
        self.push(GameCommand::cancel_timer(id));
    }
    /// Requests a later timer-state event.
    pub fn query_timer(&mut self, id: impl Into<String>, request_id: u64) {
        self.push(GameCommand::query_timer(id, request_id));
    }
    /// Registers a Behavior Tree action result.
    pub fn set_behavior_action(&mut self, id: impl Into<String>, status: GameBehaviorStatus) {
        self.push(GameCommand::set_behavior_tree_action(id, status));
    }
    /// Registers a Behavior Tree condition result.
    pub fn set_behavior_condition(&mut self, id: impl Into<String>, status: GameBehaviorStatus) {
        self.push(GameCommand::set_behavior_tree_condition(id, status));
    }
    /// Broadcasts one project-defined typed event.
    pub fn broadcast<E: ProjectEvent>(&mut self, payload: E::Payload) {
        self.output.with_mut(|output| {
            output
                .emitted_events
                .push(GameEventEmission::broadcast(E::ID, payload.to_value()));
        });
    }
    /// Sends one project-defined typed event with a generation-checked target.
    pub fn send<E: ProjectEvent>(&mut self, target: GameEntityHandle, payload: E::Payload) {
        self.output.with_mut(|output| {
            output.emitted_events.push(GameEventEmission::targeted(
                E::ID,
                target,
                payload.to_value(),
            ));
        });
    }
}

impl GameSystemParam for Commands {
    fn declare(access: &mut GameSystemAccess) {
        access.command_families.extend([
            GameCommandFamily::Transform,
            GameCommandFamily::Character,
            GameCommandFamily::Navigation,
            GameCommandFamily::BehaviorTree,
            GameCommandFamily::PrefabSpawn,
            GameCommandFamily::Despawn,
            GameCommandFamily::Component,
            GameCommandFamily::Animation,
            GameCommandFamily::Hitbox,
            GameCommandFamily::Audio,
            GameCommandFamily::LockOn,
            GameCommandFamily::Ui,
            GameCommandFamily::Scene,
            GameCommandFamily::Save,
            GameCommandFamily::Timer,
            GameCommandFamily::GameEvent,
        ]);
    }
    fn fetch(_: &GameInvocation, output: TypedOutput) -> Result<Self, GameApiError> {
        Ok(Self { output })
    }
}
impl sealed::SystemParam for Commands {}

fn decode_transform(value: &Value) -> Result<TransformView, String> {
    let fields = object(value)?;
    Ok(TransformView {
        translation: vec3(field(fields, "translation")?)?,
        rotation: quat(field(fields, "rotation")?)?,
        scale: vec3(field(fields, "scale")?)?,
    })
}

fn field<'a>(fields: &'a BTreeMap<String, Value>, name: &str) -> Result<&'a Value, String> {
    fields
        .get(name)
        .ok_or_else(|| format!("required field `{name}` is missing"))
}
fn object(value: &Value) -> Result<&BTreeMap<String, Value>, String> {
    if let Value::Object(value) = value {
        Ok(value)
    } else {
        Err("expected an object".to_owned())
    }
}
fn array(value: &Value) -> Result<&[Value], String> {
    if let Value::Array(value) = value {
        Ok(value)
    } else {
        Err("expected an array".to_owned())
    }
}
fn string(value: &Value) -> Result<&str, String> {
    if let Value::String(value) = value {
        Ok(value)
    } else {
        Err("expected a string".to_owned())
    }
}
fn boolean(value: &Value) -> Result<bool, String> {
    if let Value::Bool(value) = value {
        Ok(*value)
    } else {
        Err("expected a boolean".to_owned())
    }
}
fn number(value: &Value) -> Result<f64, String> {
    match value {
        Value::F64(value) => Ok(*value),
        Value::I64(value) => Ok(*value as f64),
        Value::U64(value) => Ok(*value as f64),
        _ => Err("expected a number".to_owned()),
    }
}
fn signed(value: &Value) -> Result<i64, String> {
    match value {
        Value::I64(value) => Ok(*value),
        Value::U64(value) => (*value)
            .try_into()
            .map_err(|_| "unsigned integer is outside i64".to_owned()),
        Value::String(value) => value
            .parse()
            .map_err(|_| "expected a signed integer string".to_owned()),
        _ => Err("expected a signed integer".to_owned()),
    }
}
fn unsigned(value: &Value) -> Result<u64, String> {
    match value {
        Value::U64(value) => Ok(*value),
        Value::I64(value) => (*value)
            .try_into()
            .map_err(|_| "expected a non-negative integer".to_owned()),
        Value::String(value) => value
            .parse()
            .map_err(|_| "expected a non-negative integer string".to_owned()),
        _ => Err("expected a non-negative integer".to_owned()),
    }
}
fn string_field<'a>(fields: &'a BTreeMap<String, Value>, name: &str) -> Result<&'a str, String> {
    string(field(fields, name)?)
}
fn bool_field(fields: &BTreeMap<String, Value>, name: &str) -> Result<bool, String> {
    boolean(field(fields, name)?)
}
fn number_field(fields: &BTreeMap<String, Value>, name: &str) -> Result<f64, String> {
    number(field(fields, name)?)
}
fn signed_field(fields: &BTreeMap<String, Value>, name: &str) -> Result<i64, String> {
    signed(field(fields, name)?)
}
fn unsigned_field(fields: &BTreeMap<String, Value>, name: &str) -> Result<u64, String> {
    unsigned(field(fields, name)?)
}
fn optional_string(value: &Value) -> Result<Option<&str>, String> {
    if matches!(value, Value::Null) {
        Ok(None)
    } else {
        string(value).map(Some)
    }
}
fn vec3(value: &Value) -> Result<Vec3, String> {
    let values = array(value)?;
    if values.len() != 3 {
        return Err(format!(
            "expected three vector elements, found {}",
            values.len()
        ));
    }
    Ok(Vec3::new(
        number(&values[0])? as f32,
        number(&values[1])? as f32,
        number(&values[2])? as f32,
    ))
}
fn quat(value: &Value) -> Result<Quat, String> {
    let values = array(value)?;
    if values.len() != 4 {
        return Err(format!(
            "expected four quaternion elements, found {}",
            values.len()
        ));
    }
    Ok(Quat::from_xyzw(
        number(&values[0])? as f32,
        number(&values[1])? as f32,
        number(&values[2])? as f32,
        number(&values[3])? as f32,
    ))
}
fn optional_vec3(value: &Value) -> Result<Option<Vec3>, String> {
    if matches!(value, Value::Null) {
        Ok(None)
    } else {
        vec3(value).map(Some)
    }
}
fn entity(value: &Value) -> Result<GameEntityHandle, String> {
    let fields = object(value)?;
    Ok(GameEntityHandle {
        id: unsigned_field(fields, "id")?
            .try_into()
            .map_err(|_| "entity ID is outside u32".to_owned())?,
        generation: unsigned_field(fields, "generation")?
            .try_into()
            .map_err(|_| "entity generation is outside u32".to_owned())?,
    })
}
fn optional_entity(value: &Value) -> Result<Option<GameEntityHandle>, String> {
    if matches!(value, Value::Null) {
        Ok(None)
    } else {
        entity(value).map(Some)
    }
}
fn string_array(value: &Value) -> Result<Vec<String>, String> {
    array(value)?
        .iter()
        .map(|value| string(value).map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Default)]
    struct Health {
        current: i64,
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct State {
        enabled: bool,
    }

    impl GameResource for State {
        const RESOURCE_ID: &'static str = "game.state";
        const DISPLAY_NAME: &'static str = "State";
        fn schema() -> crate::game_contracts::GameResourceSchema {
            unreachable!()
        }
        fn from_value(value: &Value) -> Result<Self, String> {
            Ok(Self {
                enabled: bool_field(object(value)?, "enabled")?,
            })
        }
        fn to_value(&self) -> Value {
            Value::Object(BTreeMap::from([(
                "enabled".to_owned(),
                Value::Bool(self.enabled),
            )]))
        }
    }

    struct Jump;
    impl InputAction for Jump {
        const NAME: &'static str = "jump";
    }

    impl GameComponent for Health {
        const TYPE_ID: &'static str = "game.health";
        const DISPLAY_NAME: &'static str = "Health";
        fn schema() -> crate::game_contracts::ComponentSchema {
            unreachable!()
        }
        fn from_authoring_value(value: &Value) -> Result<Self, String> {
            Ok(Self {
                current: signed_field(object(value)?, "current")?,
            })
        }
        fn to_authoring_value(&self) -> Value {
            Value::Object(BTreeMap::from([(
                "current".to_owned(),
                Value::I64(self.current),
            )]))
        }
    }

    struct HealthQuery;
    impl QuerySpec for HealthQuery {
        const ID: &'static str = "game.query.health";
        fn access() -> GameQueryAccess {
            QueryAccessBuilder::new("game.query.health")
                .write::<Health>()
                .build()
        }
    }

    #[test]
    fn typed_query_decodes_and_patches_declared_component() {
        let input = GameInvocation {
            schema_version: crate::game_io::GAME_IO_SCHEMA_VERSION,
            system_id: "game.test".to_owned(),
            clock: GameClock::default(),
            input_actions: BTreeMap::new(),
            save_values: BTreeMap::new(),
            resources: BTreeMap::new(),
            host_views: BTreeMap::new(),
            events: Vec::new(),
            queries: vec![crate::game_io::GameQueryResult {
                query_id: "game.query.health".to_owned(),
                rows: vec![GameQueryRow {
                    entity: GameEntityHandle {
                        id: 1,
                        generation: 2,
                    },
                    authoring_id: None,
                    components: BTreeMap::from([(
                        ComponentTypeId::new("game.health"),
                        Value::Object(BTreeMap::from([("current".to_owned(), Value::I64(3))])),
                    )]),
                    engine_views: BTreeMap::new(),
                }],
            }],
        };
        let output = TypedOutput::new();
        let mut query = Query::<HealthQuery>::fetch(&input, output.clone()).unwrap();
        assert_eq!(query.rows()[0].component::<Health>().unwrap().current, 3);
        query
            .set(query.rows()[0].entity(), Health { current: 4 })
            .unwrap();
        drop(query);
        assert_eq!(output.into_output().unwrap().component_patches.len(), 1);
    }

    #[test]
    fn undeclared_component_access_is_an_explicit_error() {
        struct Empty;
        impl QuerySpec for Empty {
            const ID: &'static str = "game.query.empty";
            fn access() -> GameQueryAccess {
                QueryAccessBuilder::new(Self::ID).build()
            }
        }
        let row = QueryRow::<Empty> {
            raw: GameQueryRow {
                entity: GameEntityHandle {
                    id: 1,
                    generation: 1,
                },
                authoring_id: None,
                components: BTreeMap::new(),
                engine_views: BTreeMap::new(),
            },
            access: Empty::access(),
            marker: PhantomData,
        };
        assert!(matches!(
            row.component::<Health>(),
            Err(GameApiError::UndeclaredComponent { .. })
        ));
    }

    #[test]
    fn required_resource_never_falls_back_to_default() {
        let input = GameInvocation {
            schema_version: crate::game_io::GAME_IO_SCHEMA_VERSION,
            system_id: "game.test".to_owned(),
            clock: GameClock::default(),
            input_actions: BTreeMap::new(),
            save_values: BTreeMap::new(),
            queries: Vec::new(),
            resources: BTreeMap::new(),
            host_views: BTreeMap::new(),
            events: Vec::new(),
        };

        assert!(matches!(
            Res::<State>::fetch(&input, TypedOutput::new()),
            Err(GameApiError::MissingResource("game.state"))
        ));
    }

    #[test]
    fn parameter_types_generate_one_consistent_access_manifest() {
        let mut access = GameSystemAccess::default();
        Query::<HealthQuery>::declare(&mut access);
        ResMut::<State>::declare(&mut access);
        Action::<Jump>::declare(&mut access);
        Events::<CollisionEvent>::declare(&mut access);
        Commands::declare(&mut access);

        access.validate().unwrap();
        assert_eq!(access.queries[0].id, HealthQuery::ID);
        assert_eq!(access.resources[0].id, State::RESOURCE_ID);
        assert_eq!(access.input_actions, [Jump::NAME]);
        assert_eq!(access.event_streams, [GameEventStream::Collision]);
        assert!(access.command_families.contains(&GameCommandFamily::Scene));
    }
}
