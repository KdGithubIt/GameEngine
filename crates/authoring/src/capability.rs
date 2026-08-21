//! Canonical authoring capability registry (ADR 0132).
//!
//! The registry is the single place where a semantic authoring operation is
//! declared. Editor orchestration, MCP, CLI, tests, and future structured
//! clients discover operations here instead of maintaining one capability list
//! per adapter.
//!
//! The registry describes authoring meaning only. It MUST NOT gain GUI toolkit
//! types, MCP protocol types, CLI argument parser types, provider credentials,
//! or runtime ECS behavior. Registry metadata is also descriptive rather than
//! authoritative: discovering a capability never grants permission to execute
//! it, and every mutation still passes the shared permission, validation, and
//! transaction checks owned by the domain service.
//!
//! ```
//! use engine_authoring::capability::{
//!     AuthoringCapabilityId, AuthoringCapabilityKind, AuthoringCapabilityRegistry,
//! };
//! use engine_authoring::AuthoringPermissions;
//!
//! let registry = AuthoringCapabilityRegistry::builtin();
//! let apply = registry
//!     .require(&AuthoringCapabilityId::new("scene.apply"))
//!     .expect("built-in registry declares Scene mutation");
//!
//! assert_eq!(apply.kind, AuthoringCapabilityKind::CommittedMutation);
//! // Discovery is not authorization.
//! assert!(apply.require_permission(&AuthoringPermissions::read_only()).is_err());
//! ```

use crate::{AuthoringPermission, AuthoringPermissionError, AuthoringPermissions};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt;

/// Capability namespace reserved for the generic adapter surface.
///
/// ADR 0132 requires MCP and CLI to expose `authoring.capabilities`,
/// `authoring.describe`, `authoring.inspect`, `authoring.validate`,
/// `authoring.preview`, and `authoring.apply`. Reserving the namespace here
/// keeps a registered capability from shadowing those generic operations and
/// guarantees that generic invocation cannot resolve back to itself.
pub const RESERVED_CAPABILITY_NAMESPACE: &str = "authoring";

/// Stable external identifier for one semantic authoring capability.
///
/// Capability IDs are adapter contracts. Renaming an implementation function,
/// moving a module, or changing Editor presentation MUST NOT change an ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct AuthoringCapabilityId(String);

impl AuthoringCapabilityId {
    /// Creates a capability ID from a lowercase dotted identifier.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringCapabilityIdError`] when the identifier is empty,
    /// has no dotted namespace, or contains an unsupported segment.
    pub fn try_new(value: impl Into<String>) -> Result<Self, AuthoringCapabilityIdError> {
        let value = value.into();
        validate_capability_id(&value)?;
        Ok(Self(value))
    }

    /// Creates a capability ID from a known-valid lowercase dotted identifier.
    ///
    /// # Panics
    ///
    /// Panics when `value` is not a valid capability identifier. Use
    /// [`AuthoringCapabilityId::try_new`] for untrusted input.
    pub fn new(value: impl Into<String>) -> Self {
        Self::try_new(value).expect("capability ID must be a lowercase dotted identifier")
    }

    /// Returns the identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the dot-separated identifier segments.
    ///
    /// Adapters use the segments to bind one command path or tool name per
    /// capability without inventing a second naming scheme.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }

    /// Returns whether this ID belongs to the reserved generic adapter
    /// namespace described by [`RESERVED_CAPABILITY_NAMESPACE`].
    pub fn is_reserved(&self) -> bool {
        self.0
            .split('.')
            .next()
            .is_some_and(|segment| segment == RESERVED_CAPABILITY_NAMESPACE)
    }
}

impl<'de> Deserialize<'de> for AuthoringCapabilityId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(de::Error::custom)
    }
}

impl fmt::Display for AuthoringCapabilityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Reports why a capability identifier was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthoringCapabilityIdError {
    /// The identifier was empty.
    Empty,
    /// The identifier did not contain a dotted namespace.
    MissingNamespace,
    /// One dot-separated segment was empty.
    EmptySegment,
    /// A segment contained an unsupported character or did not start with a
    /// lowercase ASCII letter.
    InvalidSegment {
        /// The rejected segment.
        segment: String,
    },
}

impl fmt::Display for AuthoringCapabilityIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("capability ID must not be empty"),
            Self::MissingNamespace => {
                formatter.write_str("capability ID must use a dotted namespace")
            }
            Self::EmptySegment => formatter.write_str("capability ID segments must not be empty"),
            Self::InvalidSegment { segment } => write!(
                formatter,
                "capability ID segment `{segment}` must start with a lowercase ASCII letter and contain only lowercase letters, digits, or underscores"
            ),
        }
    }
}

impl std::error::Error for AuthoringCapabilityIdError {}

/// Authoring domain or document family that owns a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoringDomain {
    /// Project-level identity and discovery.
    Project,
    /// Scene documents, entities, hierarchy, and components.
    Scene,
    /// Component schema discovery supplied by the authoring host.
    ComponentSchema,
    /// Asset catalog discovery and inspection.
    Asset,
    /// Prefab assets and prefab instantiation into a Scene.
    Prefab,
    /// Domain-neutral semantic graphs.
    Graph,
    /// Graph presentation documents.
    GraphView,
    /// Declarative UI documents.
    Ui,
    /// VFX effect documents.
    Vfx,
    /// Behavior Tree graph domain operations.
    BehaviorTree,
    /// Material asset documents.
    Material,
    /// Project Settings document.
    ProjectSettings,
    /// Animation Set asset documents.
    AnimationSet,
    /// Native 2D sprite, animation, and tile asset documents.
    Native2d,
    /// Timeline / Sequencer asset documents.
    Timeline,
}

/// Authoring document a capability reads or mutates.
///
/// Adapters use the ordered document list to bind their own document
/// selection, such as CLI file arguments, without duplicating domain rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoringDocumentKind {
    /// The open project root and its configuration.
    ProjectRoot,
    /// A Scene document.
    Scene,
    /// The component schema registry supplied by the authoring host.
    ComponentSchemaRegistry,
    /// The project asset manifest.
    AssetManifest,
    /// A prefab asset document.
    PrefabAsset,
    /// A semantic Graph document.
    Graph,
    /// A GraphView presentation document.
    GraphView,
    /// A declarative UI document.
    UiDocument,
    /// A VFX effect document.
    VfxEffect,
    /// A Material asset document.
    MaterialAsset,
    /// The project-wide Project Settings document.
    ProjectSettings,
    /// An Animation Set asset document.
    AnimationSet,
    /// A Native 2D Sprite Atlas asset document.
    SpriteAtlas,
    /// A Native 2D Sprite Animation asset document.
    SpriteAnimation,
    /// A Native 2D Tile Set asset document.
    TileSet,
    /// A Native 2D Tile Map asset document.
    TileMap,
    /// A Timeline / Sequencer asset document.
    Timeline,
}

/// Semantic shape of one capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoringCapabilityKind {
    /// Reads authoring state without computing a mutation.
    Query,
    /// Validates authoring state and returns structured diagnostics.
    Validation,
    /// Computes diagnostics and a semantic diff without committing.
    PreviewMutation,
    /// Commits one atomic transaction to authoring data.
    CommittedMutation,
    /// A domain operation that is not expressible as one of the generic
    /// inspect, validate, preview, or apply shapes, such as compilation or
    /// deterministic layout generation.
    Operation,
}

/// How structured clients are expected to reach a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoringCapabilityExposure {
    /// Callable through the registry-driven generic authoring surface.
    Generic,
    /// Requires an explicitly declared specialized adapter operation because
    /// its meaning is not usefully represented by the generic cycle.
    Specialized,
    /// Deliberately excluded from structured AI authoring by an Accepted ADR.
    ScopedOut {
        /// ADR that scopes this capability out, such as `ADR 0035`.
        adr: String,
    },
}

/// Transaction and stale-revision contract for one capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoringTransactionRequirement {
    /// Read-only operation that needs no revision base.
    None,
    /// Operates on a document supplied by the caller, so no live-document
    /// revision base applies and the owning host still controls persistence.
    DocumentScoped,
    /// Requires an exact revision and generation base against the live
    /// document but never commits.
    RevisionChecked,
    /// Requires an exact revision and generation base and commits one atomic
    /// transaction when blocking validation succeeds.
    AtomicCommit,
}

/// Machine-readable description of a capability payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthoringSchemaRef {
    /// Shared authoring type that defines the payload, when one exists.
    pub type_name: Option<String>,
    /// JSON schema for structured clients.
    ///
    /// Input schemas are exact for every registered capability, generic and
    /// specialized alike, so an adapter never hand-writes a second argument
    /// contract that can disagree with this one.
    pub json_schema: Value,
}

impl AuthoringSchemaRef {
    /// Describes an operation that takes or returns no payload.
    pub fn empty() -> Self {
        Self {
            type_name: None,
            json_schema: empty_object_schema(),
        }
    }

    /// Describes a payload by shared authoring type without a precise schema.
    pub fn of_type(type_name: impl Into<String>) -> Self {
        Self {
            type_name: Some(type_name.into()),
            json_schema: json!({"type": "object"}),
        }
    }

    /// Describes a payload by JSON schema without a single shared type.
    pub fn json(json_schema: Value) -> Self {
        Self {
            type_name: None,
            json_schema,
        }
    }

    /// Describes a payload by shared authoring type and JSON schema.
    pub fn typed_json(type_name: impl Into<String>, json_schema: Value) -> Self {
        Self {
            type_name: Some(type_name.into()),
            json_schema,
        }
    }

    /// Describes a bulk command batch applied against an exact document base.
    pub fn command_batch(command_type: impl Into<String>) -> Self {
        let command_type = command_type.into();
        let json_schema = json!({
            "type": "object",
            "required": ["expected_revision", "expected_generation", "commands"],
            "properties": {
                "expected_revision": {"type": "integer", "minimum": 0},
                "expected_generation": {"type": "integer", "minimum": 0},
                "commands": {
                    "type": "array",
                    "items": {"type": "object", "title": command_type.clone()}
                }
            },
            "additionalProperties": false
        });
        Self {
            type_name: Some(command_type),
            json_schema,
        }
    }
}

/// One registered semantic authoring capability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthoringCapability {
    /// Stable external capability identifier.
    pub id: AuthoringCapabilityId,
    /// Authoring domain that owns the capability.
    pub domain: AuthoringDomain,
    /// Semantic shape of the operation.
    pub kind: AuthoringCapabilityKind,
    /// How structured clients reach the capability.
    pub exposure: AuthoringCapabilityExposure,
    /// Documents the capability reads or mutates, in adapter argument order.
    pub documents: Vec<AuthoringDocumentKind>,
    /// Input contract.
    pub input: AuthoringSchemaRef,
    /// Output contract.
    pub output: AuthoringSchemaRef,
    /// Permission required at the shared authoring boundary.
    pub permission: AuthoringPermission,
    /// Transaction and stale-revision contract.
    pub transaction: AuthoringTransactionRequirement,
    /// Human- and AI-readable description.
    pub description: String,
}

/// Context-efficient discovery view of one registered capability.
///
/// This summary is derived from [`AuthoringCapability`] and intentionally omits
/// payload schemas, document contracts, permissions, and transaction metadata.
/// Structured clients use it to choose a capability before requesting the full
/// descriptor on demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoringCapabilitySummary {
    /// Stable external capability identifier.
    pub id: AuthoringCapabilityId,
    /// Authoring domain that owns the capability.
    pub domain: AuthoringDomain,
    /// Semantic shape of the operation.
    pub kind: AuthoringCapabilityKind,
    /// How structured clients reach the capability.
    pub exposure: AuthoringCapabilityExposure,
    /// Human- and AI-readable description used to choose a capability.
    pub description: String,
}

impl From<&AuthoringCapability> for AuthoringCapabilitySummary {
    fn from(capability: &AuthoringCapability) -> Self {
        Self {
            id: capability.id.clone(),
            domain: capability.domain,
            kind: capability.kind,
            exposure: capability.exposure.clone(),
            description: capability.description.clone(),
        }
    }
}

impl AuthoringCapability {
    /// Requires the capability permission before execution.
    ///
    /// Adapters MAY call this for an early structured rejection, but the
    /// shared domain service remains the enforcement point.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringPermissionError`] when the permission is absent.
    pub fn require_permission(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<(), AuthoringPermissionError> {
        permissions.require(self.permission)
    }

    /// Returns whether the capability is reachable through the generic
    /// registry-driven authoring surface.
    pub fn is_generic(&self) -> bool {
        self.exposure == AuthoringCapabilityExposure::Generic
    }
}

/// Reports why a capability registry operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthoringCapabilityError {
    /// The capability ID is already registered.
    Duplicate {
        /// The rejected capability ID.
        id: AuthoringCapabilityId,
    },
    /// No capability is registered under the requested ID.
    Unknown {
        /// The requested capability ID.
        id: AuthoringCapabilityId,
    },
    /// The capability ID uses the reserved generic adapter namespace.
    ReservedNamespace {
        /// The rejected capability ID.
        id: AuthoringCapabilityId,
    },
    /// The session does not hold the permission the capability requires.
    Permission(AuthoringPermissionError),
}

impl AuthoringCapabilityError {
    /// Returns the stable diagnostic-style code exposed to structured clients.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Duplicate { .. } => "authoring.capability_duplicate",
            Self::Unknown { .. } => "authoring.capability_unknown",
            Self::ReservedNamespace { .. } => "authoring.capability_reserved_namespace",
            Self::Permission(error) => error.code(),
        }
    }
}

impl From<AuthoringPermissionError> for AuthoringCapabilityError {
    fn from(error: AuthoringPermissionError) -> Self {
        Self::Permission(error)
    }
}

impl fmt::Display for AuthoringCapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate { id } => write!(formatter, "capability `{id}` is already registered"),
            Self::Unknown { id } => write!(formatter, "unknown authoring capability `{id}`"),
            Self::ReservedNamespace { id } => write!(
                formatter,
                "capability `{id}` may not use the reserved `{RESERVED_CAPABILITY_NAMESPACE}` namespace"
            ),
            Self::Permission(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AuthoringCapabilityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Permission(error) => Some(error),
            Self::Duplicate { .. } | Self::Unknown { .. } | Self::ReservedNamespace { .. } => None,
        }
    }
}

/// Deterministic catalog of semantic authoring capabilities.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AuthoringCapabilityRegistry {
    capabilities: BTreeMap<AuthoringCapabilityId, AuthoringCapability>,
}

impl AuthoringCapabilityRegistry {
    /// Creates a registry without capabilities.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Creates the registry of capabilities owned by this crate's shared
    /// authoring services.
    ///
    /// # Panics
    ///
    /// Panics when the built-in declarations contain a duplicate or reserved
    /// capability ID, which is a programming error in this module rather than
    /// a recoverable failure.
    pub fn builtin() -> Self {
        let mut registry = Self::empty();
        for capability in builtin_capabilities() {
            registry
                .register(capability)
                .expect("built-in capability declarations must be unique and unreserved");
        }
        registry
    }

    /// Registers one capability.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringCapabilityError`] when the ID is already registered
    /// or uses the reserved generic adapter namespace.
    pub fn register(
        &mut self,
        capability: AuthoringCapability,
    ) -> Result<(), AuthoringCapabilityError> {
        if capability.id.is_reserved() {
            return Err(AuthoringCapabilityError::ReservedNamespace { id: capability.id });
        }
        if self.capabilities.contains_key(&capability.id) {
            return Err(AuthoringCapabilityError::Duplicate { id: capability.id });
        }
        self.capabilities.insert(capability.id.clone(), capability);
        Ok(())
    }

    /// Returns the capability registered under `id`.
    pub fn get(&self, id: &AuthoringCapabilityId) -> Option<&AuthoringCapability> {
        self.capabilities.get(id)
    }

    /// Returns the capability registered under `id`.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringCapabilityError::Unknown`] when nothing is
    /// registered under `id`.
    pub fn require(
        &self,
        id: &AuthoringCapabilityId,
    ) -> Result<&AuthoringCapability, AuthoringCapabilityError> {
        self.get(id)
            .ok_or_else(|| AuthoringCapabilityError::Unknown { id: id.clone() })
    }

    /// Requires the permission declared for `id` before an adapter executes it.
    ///
    /// Adapters whose shared service does not itself receive the session
    /// permission set use this so their check is the registry contract rather
    /// than a permission constant retyped in adapter code.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringCapabilityError`] when the capability is unknown or
    /// the session lacks the declared permission.
    pub fn authorize(
        &self,
        id: &AuthoringCapabilityId,
        permissions: &AuthoringPermissions,
    ) -> Result<&AuthoringCapability, AuthoringCapabilityError> {
        let capability = self.require(id)?;
        capability.require_permission(permissions)?;
        Ok(capability)
    }

    /// Returns every capability in deterministic capability-ID order.
    pub fn capabilities(&self) -> impl Iterator<Item = &AuthoringCapability> {
        self.capabilities.values()
    }

    /// Returns context-efficient summaries in deterministic capability-ID order.
    ///
    /// Summaries are always projected from the canonical descriptors, so adapters
    /// never maintain a second capability catalog for compact discovery.
    pub fn summaries(&self) -> impl Iterator<Item = AuthoringCapabilitySummary> + '_ {
        self.capabilities().map(AuthoringCapabilitySummary::from)
    }

    /// Returns capabilities owned by one authoring domain.
    pub fn domain(&self, domain: AuthoringDomain) -> impl Iterator<Item = &AuthoringCapability> {
        self.capabilities()
            .filter(move |capability| capability.domain == domain)
    }

    /// Returns the number of registered capabilities.
    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    /// Returns whether no capability is registered.
    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }
}

fn empty_object_schema() -> Value {
    json!({"type": "object", "properties": {}, "additionalProperties": false})
}

fn query(
    id: &str,
    domain: AuthoringDomain,
    documents: Vec<AuthoringDocumentKind>,
    output: AuthoringSchemaRef,
    description: &str,
) -> AuthoringCapability {
    AuthoringCapability {
        id: AuthoringCapabilityId::new(id),
        domain,
        kind: AuthoringCapabilityKind::Query,
        exposure: AuthoringCapabilityExposure::Generic,
        documents,
        input: AuthoringSchemaRef::empty(),
        output,
        permission: AuthoringPermission::Read,
        transaction: AuthoringTransactionRequirement::None,
        description: description.to_owned(),
    }
}

fn validation(
    id: &str,
    domain: AuthoringDomain,
    documents: Vec<AuthoringDocumentKind>,
    output: AuthoringSchemaRef,
    description: &str,
) -> AuthoringCapability {
    AuthoringCapability {
        kind: AuthoringCapabilityKind::Validation,
        ..query(id, domain, documents, output, description)
    }
}

fn preview(
    id: &str,
    domain: AuthoringDomain,
    documents: Vec<AuthoringDocumentKind>,
    command_type: &str,
    output: AuthoringSchemaRef,
    description: &str,
) -> AuthoringCapability {
    AuthoringCapability {
        kind: AuthoringCapabilityKind::PreviewMutation,
        input: AuthoringSchemaRef::command_batch(command_type),
        permission: AuthoringPermission::Preview,
        transaction: AuthoringTransactionRequirement::RevisionChecked,
        ..query(id, domain, documents, output, description)
    }
}

fn commit(
    id: &str,
    domain: AuthoringDomain,
    documents: Vec<AuthoringDocumentKind>,
    command_type: &str,
    output: AuthoringSchemaRef,
    description: &str,
) -> AuthoringCapability {
    AuthoringCapability {
        kind: AuthoringCapabilityKind::CommittedMutation,
        permission: AuthoringPermission::ProjectDataWrite,
        transaction: AuthoringTransactionRequirement::AtomicCommit,
        ..preview(id, domain, documents, command_type, output, description)
    }
}

fn with_input(capability: AuthoringCapability, input: AuthoringSchemaRef) -> AuthoringCapability {
    AuthoringCapability {
        input,
        ..capability
    }
}

/// Declares a capability that structured clients reach through an explicitly
/// declared specialized adapter operation.
fn specialized(
    id: &str,
    domain: AuthoringDomain,
    kind: AuthoringCapabilityKind,
    documents: Vec<AuthoringDocumentKind>,
    input: AuthoringSchemaRef,
    output: AuthoringSchemaRef,
    description: &str,
) -> AuthoringCapability {
    let (permission, transaction) = match kind {
        AuthoringCapabilityKind::Query
        | AuthoringCapabilityKind::Validation
        | AuthoringCapabilityKind::Operation => (
            AuthoringPermission::Read,
            AuthoringTransactionRequirement::None,
        ),
        AuthoringCapabilityKind::PreviewMutation => (
            AuthoringPermission::Preview,
            AuthoringTransactionRequirement::DocumentScoped,
        ),
        AuthoringCapabilityKind::CommittedMutation => (
            AuthoringPermission::ProjectDataWrite,
            AuthoringTransactionRequirement::DocumentScoped,
        ),
    };
    AuthoringCapability {
        id: AuthoringCapabilityId::new(id),
        domain,
        kind,
        exposure: AuthoringCapabilityExposure::Specialized,
        documents,
        input,
        output,
        permission,
        transaction,
        description: description.to_owned(),
    }
}

fn document_schema(field: &str) -> AuthoringSchemaRef {
    AuthoringSchemaRef::json(json!({
        "type": "object",
        "required": [field],
        "properties": {field: {"type": "object"}},
        "additionalProperties": false
    }))
}

/// Describes a caller-supplied document plus the command batch applied to it.
fn document_command_schema(field: &str, command_type: &str) -> AuthoringSchemaRef {
    AuthoringSchemaRef::typed_json(
        command_type,
        json!({
            "type": "object",
            "required": [field, "commands"],
            "properties": {
                field: {"type": "object"},
                "commands": {
                    "type": "array",
                    "items": {"type": "object", "title": command_type}
                }
            },
            "additionalProperties": false
        }),
    )
}

fn prefab_instantiation_schema() -> AuthoringSchemaRef {
    AuthoringSchemaRef::typed_json(
        "PrefabInstantiationRequest",
        json!({
            "type": "object",
            "required": ["source", "expected_revision", "expected_generation"],
            "properties": {
                "source": {"type": "string"},
                "parent": {"type": ["string", "null"]},
                "expected_revision": {"type": "integer", "minimum": 0},
                "expected_generation": {"type": "integer", "minimum": 0}
            },
            "additionalProperties": false
        }),
    )
}

fn scene_documents() -> Vec<AuthoringDocumentKind> {
    vec![AuthoringDocumentKind::Scene]
}

fn graph_documents() -> Vec<AuthoringDocumentKind> {
    vec![AuthoringDocumentKind::Graph]
}

fn graph_view_documents() -> Vec<AuthoringDocumentKind> {
    vec![
        AuthoringDocumentKind::Graph,
        AuthoringDocumentKind::GraphView,
    ]
}

fn ui_documents() -> Vec<AuthoringDocumentKind> {
    vec![AuthoringDocumentKind::UiDocument]
}

fn typed_document_replace_schema(document_type: &str) -> AuthoringSchemaRef {
    AuthoringSchemaRef::typed_json(
        document_type,
        json!({
            "type": "object",
            "required": ["expected_revision", "expected_generation", "replacement"],
            "properties": {
                "expected_revision": {"type": "integer", "minimum": 0},
                "expected_generation": {"type": "integer", "minimum": 0},
                "replacement": {"type": "object", "title": document_type}
            },
            "additionalProperties": false
        }),
    )
}

fn builtin_capabilities() -> Vec<AuthoringCapability> {
    let mut capabilities = vec![
        query(
            "project.describe",
            AuthoringDomain::Project,
            vec![AuthoringDocumentKind::ProjectRoot],
            AuthoringSchemaRef::of_type("ProjectConfig"),
            "Describe the active GameEngine project and structured authoring capabilities.",
        ),
        query(
            "scene.inspect",
            AuthoringDomain::Scene,
            scene_documents(),
            AuthoringSchemaRef::of_type("SceneAuthoringSnapshot"),
            "Inspect the live committed Scene with revision and generation tokens.",
        ),
        validation(
            "scene.validate",
            AuthoringDomain::Scene,
            scene_documents(),
            AuthoringSchemaRef::of_type("SceneAuthoringValidation"),
            "Validate the live committed Scene without mutation.",
        ),
        preview(
            "scene.preview",
            AuthoringDomain::Scene,
            scene_documents(),
            "AuthoringCommand",
            AuthoringSchemaRef::of_type("SceneAuthoringMutation"),
            "Preview one atomic Scene command batch against an exact revision/generation base.",
        ),
        commit(
            "scene.apply",
            AuthoringDomain::Scene,
            scene_documents(),
            "AuthoringCommand",
            AuthoringSchemaRef::of_type("SceneAuthoringMutation"),
            "Apply one atomic Scene command batch through the shared authoring transaction boundary.",
        ),
        with_input(
            query(
                "entity.find",
                AuthoringDomain::Scene,
                scene_documents(),
                AuthoringSchemaRef::of_type("AuthoringEntity"),
                "Find live Scene entities by stable ID, slug, display name, or description.",
            ),
            AuthoringSchemaRef::json(json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "additionalProperties": false
            })),
        ),
        with_input(
            query(
                "entity.inspect",
                AuthoringDomain::Scene,
                scene_documents(),
                AuthoringSchemaRef::of_type("AuthoringEntity"),
                "Inspect one live Scene entity by stable ID.",
            ),
            AuthoringSchemaRef::json(json!({
                "type": "object",
                "required": ["entity"],
                "properties": {"entity": {"type": "string"}},
                "additionalProperties": false
            })),
        ),
        query(
            "component.schemas",
            AuthoringDomain::ComponentSchema,
            vec![AuthoringDocumentKind::ComponentSchemaRegistry],
            AuthoringSchemaRef::of_type("ComponentSchema"),
            "Discover component schemas supplied by the live authoring host.",
        ),
        query(
            "graph.inspect",
            AuthoringDomain::Graph,
            graph_documents(),
            AuthoringSchemaRef::of_type("GraphAuthoringSnapshot"),
            "Inspect the active semantic Graph.",
        ),
        validation(
            "graph.validate",
            AuthoringDomain::Graph,
            graph_documents(),
            AuthoringSchemaRef::of_type("GraphAuthoringValidation"),
            "Validate the active semantic Graph with its built-in domain.",
        ),
        preview(
            "graph.preview",
            AuthoringDomain::Graph,
            graph_documents(),
            "GraphCommand",
            AuthoringSchemaRef::of_type("GraphAuthoringMutation"),
            "Preview one atomic semantic Graph command batch.",
        ),
        commit(
            "graph.apply",
            AuthoringDomain::Graph,
            graph_documents(),
            "GraphCommand",
            AuthoringSchemaRef::of_type("GraphAuthoringMutation"),
            "Apply one atomic semantic Graph command batch.",
        ),
        query(
            "graph.layout.inspect",
            AuthoringDomain::GraphView,
            graph_view_documents(),
            AuthoringSchemaRef::of_type("GraphViewAuthoringSnapshot"),
            "Inspect the active GraphView presentation document.",
        ),
        validation(
            "graph.layout.validate",
            AuthoringDomain::GraphView,
            graph_view_documents(),
            AuthoringSchemaRef::of_type("GraphViewAuthoringValidation"),
            "Validate the active GraphView against its semantic Graph.",
        ),
        preview(
            "graph.layout.preview",
            AuthoringDomain::GraphView,
            graph_view_documents(),
            "GraphViewCommand",
            AuthoringSchemaRef::of_type("GraphViewAuthoringMutation"),
            "Preview one atomic GraphView presentation command batch.",
        ),
        commit(
            "graph.layout.apply",
            AuthoringDomain::GraphView,
            graph_view_documents(),
            "GraphViewCommand",
            AuthoringSchemaRef::of_type("GraphViewAuthoringMutation"),
            "Apply one atomic GraphView presentation command batch.",
        ),
        query(
            "ui.inspect",
            AuthoringDomain::Ui,
            ui_documents(),
            AuthoringSchemaRef::of_type("UiAuthoringSnapshot"),
            "Inspect the active declarative UI document.",
        ),
        validation(
            "ui.validate",
            AuthoringDomain::Ui,
            ui_documents(),
            AuthoringSchemaRef::of_type("UiAuthoringValidation"),
            "Validate the active declarative UI document.",
        ),
        preview(
            "ui.preview",
            AuthoringDomain::Ui,
            ui_documents(),
            "UiDocumentCommand",
            AuthoringSchemaRef::of_type("UiAuthoringMutation"),
            "Preview one atomic declarative UI command batch.",
        ),
        commit(
            "ui.apply",
            AuthoringDomain::Ui,
            ui_documents(),
            "UiDocumentCommand",
            AuthoringSchemaRef::of_type("UiAuthoringMutation"),
            "Apply one atomic declarative UI command batch.",
        ),
        query(
            "material.inspect",
            AuthoringDomain::Material,
            vec![AuthoringDocumentKind::MaterialAsset],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringSnapshot<MaterialAsset>"),
            "Inspect the active Material through the shared typed-document boundary.",
        ),
        validation(
            "material.validate",
            AuthoringDomain::Material,
            vec![AuthoringDocumentKind::MaterialAsset],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringValidation"),
            "Validate the active Material through the shared typed-document boundary.",
        ),
        with_input(
            preview(
                "material.preview",
                AuthoringDomain::Material,
                vec![AuthoringDocumentKind::MaterialAsset],
                "MaterialAsset",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<MaterialAsset>"),
                "Preview one atomic Material replacement.",
            ),
            typed_document_replace_schema("MaterialAsset"),
        ),
        with_input(
            commit(
                "material.apply",
                AuthoringDomain::Material,
                vec![AuthoringDocumentKind::MaterialAsset],
                "MaterialAsset",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<MaterialAsset>"),
                "Apply one atomic Material replacement.",
            ),
            typed_document_replace_schema("MaterialAsset"),
        ),
        query(
            "project_settings.inspect",
            AuthoringDomain::ProjectSettings,
            vec![AuthoringDocumentKind::ProjectSettings],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringSnapshot<ProjectSettings>"),
            "Inspect Project Settings through the shared typed-document boundary.",
        ),
        validation(
            "project_settings.validate",
            AuthoringDomain::ProjectSettings,
            vec![AuthoringDocumentKind::ProjectSettings],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringValidation"),
            "Validate Project Settings through the shared typed-document boundary.",
        ),
        with_input(
            preview(
                "project_settings.preview",
                AuthoringDomain::ProjectSettings,
                vec![AuthoringDocumentKind::ProjectSettings],
                "ProjectSettings",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<ProjectSettings>"),
                "Preview one atomic Project Settings replacement.",
            ),
            typed_document_replace_schema("ProjectSettings"),
        ),
        with_input(
            commit(
                "project_settings.apply",
                AuthoringDomain::ProjectSettings,
                vec![AuthoringDocumentKind::ProjectSettings],
                "ProjectSettings",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<ProjectSettings>"),
                "Apply one atomic Project Settings replacement.",
            ),
            typed_document_replace_schema("ProjectSettings"),
        ),
        query(
            "animation_set.inspect",
            AuthoringDomain::AnimationSet,
            vec![AuthoringDocumentKind::AnimationSet],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringSnapshot<AnimationSet>"),
            "Inspect the active Animation Set through the shared typed-document boundary.",
        ),
        validation(
            "animation_set.validate",
            AuthoringDomain::AnimationSet,
            vec![AuthoringDocumentKind::AnimationSet],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringValidation"),
            "Validate the active Animation Set through the shared typed-document boundary.",
        ),
        with_input(
            preview(
                "animation_set.preview",
                AuthoringDomain::AnimationSet,
                vec![AuthoringDocumentKind::AnimationSet],
                "AnimationSet",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<AnimationSet>"),
                "Preview one atomic Animation Set replacement.",
            ),
            typed_document_replace_schema("AnimationSet"),
        ),
        with_input(
            commit(
                "animation_set.apply",
                AuthoringDomain::AnimationSet,
                vec![AuthoringDocumentKind::AnimationSet],
                "AnimationSet",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<AnimationSet>"),
                "Apply one atomic Animation Set replacement.",
            ),
            typed_document_replace_schema("AnimationSet"),
        ),
        query(
            "sprite_atlas.inspect",
            AuthoringDomain::Native2d,
            vec![AuthoringDocumentKind::SpriteAtlas],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringSnapshot<SpriteAtlasDocument>"),
            "Inspect a Native 2D Sprite Atlas through the shared typed-document boundary.",
        ),
        validation(
            "sprite_atlas.validate",
            AuthoringDomain::Native2d,
            vec![AuthoringDocumentKind::SpriteAtlas],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringValidation"),
            "Validate a Native 2D Sprite Atlas through the shared typed-document boundary.",
        ),
        with_input(
            preview(
                "sprite_atlas.preview",
                AuthoringDomain::Native2d,
                vec![AuthoringDocumentKind::SpriteAtlas],
                "SpriteAtlasDocument",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<SpriteAtlasDocument>"),
                "Preview one atomic Sprite Atlas replacement.",
            ),
            typed_document_replace_schema("SpriteAtlasDocument"),
        ),
        with_input(
            commit(
                "sprite_atlas.apply",
                AuthoringDomain::Native2d,
                vec![AuthoringDocumentKind::SpriteAtlas],
                "SpriteAtlasDocument",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<SpriteAtlasDocument>"),
                "Apply one atomic Sprite Atlas replacement.",
            ),
            typed_document_replace_schema("SpriteAtlasDocument"),
        ),
        query(
            "sprite_animation.inspect",
            AuthoringDomain::Native2d,
            vec![AuthoringDocumentKind::SpriteAnimation],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringSnapshot<SpriteAnimationDocument>"),
            "Inspect a Native 2D Sprite Animation through the shared typed-document boundary.",
        ),
        validation(
            "sprite_animation.validate",
            AuthoringDomain::Native2d,
            vec![AuthoringDocumentKind::SpriteAnimation],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringValidation"),
            "Validate a Native 2D Sprite Animation through the shared typed-document boundary.",
        ),
        with_input(
            preview(
                "sprite_animation.preview",
                AuthoringDomain::Native2d,
                vec![AuthoringDocumentKind::SpriteAnimation],
                "SpriteAnimationDocument",
                AuthoringSchemaRef::of_type(
                    "TypedDocumentAuthoringMutation<SpriteAnimationDocument>",
                ),
                "Preview one atomic Sprite Animation replacement.",
            ),
            typed_document_replace_schema("SpriteAnimationDocument"),
        ),
        with_input(
            commit(
                "sprite_animation.apply",
                AuthoringDomain::Native2d,
                vec![AuthoringDocumentKind::SpriteAnimation],
                "SpriteAnimationDocument",
                AuthoringSchemaRef::of_type(
                    "TypedDocumentAuthoringMutation<SpriteAnimationDocument>",
                ),
                "Apply one atomic Sprite Animation replacement.",
            ),
            typed_document_replace_schema("SpriteAnimationDocument"),
        ),
        query(
            "tile_set.inspect",
            AuthoringDomain::Native2d,
            vec![AuthoringDocumentKind::TileSet],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringSnapshot<TileSetDocument>"),
            "Inspect a Native 2D Tile Set through the shared typed-document boundary.",
        ),
        validation(
            "tile_set.validate",
            AuthoringDomain::Native2d,
            vec![AuthoringDocumentKind::TileSet],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringValidation"),
            "Validate a Native 2D Tile Set through the shared typed-document boundary.",
        ),
        with_input(
            preview(
                "tile_set.preview",
                AuthoringDomain::Native2d,
                vec![AuthoringDocumentKind::TileSet],
                "TileSetDocument",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<TileSetDocument>"),
                "Preview one atomic Tile Set replacement.",
            ),
            typed_document_replace_schema("TileSetDocument"),
        ),
        with_input(
            commit(
                "tile_set.apply",
                AuthoringDomain::Native2d,
                vec![AuthoringDocumentKind::TileSet],
                "TileSetDocument",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<TileSetDocument>"),
                "Apply one atomic Tile Set replacement.",
            ),
            typed_document_replace_schema("TileSetDocument"),
        ),
        query(
            "tile_map.inspect",
            AuthoringDomain::Native2d,
            vec![AuthoringDocumentKind::TileMap],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringSnapshot<TileMapDocument>"),
            "Inspect a Native 2D Tile Map through the shared typed-document boundary.",
        ),
        validation(
            "tile_map.validate",
            AuthoringDomain::Native2d,
            vec![AuthoringDocumentKind::TileMap],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringValidation"),
            "Validate a Native 2D Tile Map through the shared typed-document boundary.",
        ),
        with_input(
            preview(
                "tile_map.preview",
                AuthoringDomain::Native2d,
                vec![AuthoringDocumentKind::TileMap],
                "TileMapDocument",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<TileMapDocument>"),
                "Preview one atomic sparse Tile Map replacement.",
            ),
            typed_document_replace_schema("TileMapDocument"),
        ),
        with_input(
            commit(
                "tile_map.apply",
                AuthoringDomain::Native2d,
                vec![AuthoringDocumentKind::TileMap],
                "TileMapDocument",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<TileMapDocument>"),
                "Apply one atomic sparse Tile Map replacement.",
            ),
            typed_document_replace_schema("TileMapDocument"),
        ),
        query(
            "timeline.inspect",
            AuthoringDomain::Timeline,
            vec![AuthoringDocumentKind::Timeline],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringSnapshot<TimelineDocument>"),
            "Inspect a Timeline through the shared typed-document boundary.",
        ),
        validation(
            "timeline.validate",
            AuthoringDomain::Timeline,
            vec![AuthoringDocumentKind::Timeline],
            AuthoringSchemaRef::of_type("TypedDocumentAuthoringValidation"),
            "Validate a Timeline through the shared typed-document boundary.",
        ),
        with_input(
            preview(
                "timeline.preview",
                AuthoringDomain::Timeline,
                vec![AuthoringDocumentKind::Timeline],
                "TimelineDocument",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<TimelineDocument>"),
                "Preview one atomic Timeline replacement.",
            ),
            typed_document_replace_schema("TimelineDocument"),
        ),
        with_input(
            commit(
                "timeline.apply",
                AuthoringDomain::Timeline,
                vec![AuthoringDocumentKind::Timeline],
                "TimelineDocument",
                AuthoringSchemaRef::of_type("TypedDocumentAuthoringMutation<TimelineDocument>"),
                "Apply one atomic Timeline replacement.",
            ),
            typed_document_replace_schema("TimelineDocument"),
        ),
    ];
    capabilities.extend(specialized_capabilities());
    capabilities
}

/// Capabilities whose meaning is not usefully represented by the generic
/// inspect/validate/preview/apply cycle.
///
/// They keep an explicitly declared specialized adapter operation, but their
/// identity, description, argument schema, permission, and transaction contract
/// are declared here so discovery, adapter descriptors, and parity coverage all
/// read one canonical list.
fn specialized_capabilities() -> Vec<AuthoringCapability> {
    let prefab_documents = vec![
        AuthoringDocumentKind::Scene,
        AuthoringDocumentKind::PrefabAsset,
    ];
    let vfx_documents = vec![AuthoringDocumentKind::VfxEffect];
    vec![
        specialized(
            "asset.search",
            AuthoringDomain::Asset,
            AuthoringCapabilityKind::Query,
            vec![AuthoringDocumentKind::AssetManifest],
            AuthoringSchemaRef::json(json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "additionalProperties": false
            })),
            AuthoringSchemaRef::of_type("AssetCatalogSearch"),
            "Search project assets and imported sub-assets by stable ID, name, path, or imported kind.",
        ),
        specialized(
            "asset.inspect",
            AuthoringDomain::Asset,
            AuthoringCapabilityKind::Query,
            vec![AuthoringDocumentKind::AssetManifest],
            AuthoringSchemaRef::json(json!({
                "type": "object",
                "required": ["asset_id"],
                "properties": {"asset_id": {"type": "string"}},
                "additionalProperties": false
            })),
            AuthoringSchemaRef::of_type("AssetInspection"),
            "Inspect one project asset by stable AssetId, including source import metadata and file state.",
        ),
        AuthoringCapability {
            permission: AuthoringPermission::AssetWrite,
            ..specialized(
                "prefab.create",
                AuthoringDomain::Prefab,
                AuthoringCapabilityKind::CommittedMutation,
                prefab_documents.clone(),
                AuthoringSchemaRef::json(json!({
                    "type": "object",
                    "required": ["root_entity", "destination"],
                    "properties": {
                        "root_entity": {"type": "string"},
                        "destination": {"type": "string"}
                    },
                    "additionalProperties": false
                })),
                AuthoringSchemaRef::of_type("PrefabCreation"),
                "Create a prefab from one Scene entity and all descendants, then register it with a fresh AssetId.",
            )
        },
        AuthoringCapability {
            transaction: AuthoringTransactionRequirement::RevisionChecked,
            ..specialized(
                "prefab.preview",
                AuthoringDomain::Prefab,
                AuthoringCapabilityKind::PreviewMutation,
                prefab_documents.clone(),
                prefab_instantiation_schema(),
                AuthoringSchemaRef::of_type("PrefabInstantiationMutation"),
                "Preview one prefab instantiation against an exact Scene revision/generation without mutating it.",
            )
        },
        AuthoringCapability {
            transaction: AuthoringTransactionRequirement::AtomicCommit,
            ..specialized(
                "prefab.instantiate",
                AuthoringDomain::Prefab,
                AuthoringCapabilityKind::CommittedMutation,
                prefab_documents,
                prefab_instantiation_schema(),
                AuthoringSchemaRef::of_type("PrefabInstantiationMutation"),
                "Instantiate a prefab into the live Scene as one validated transaction and one undo entry.",
            )
        },
        specialized(
            "vfx.schemas",
            AuthoringDomain::Vfx,
            AuthoringCapabilityKind::Query,
            vfx_documents.clone(),
            AuthoringSchemaRef::empty(),
            AuthoringSchemaRef::of_type("VfxSchemaCatalog"),
            "Discover stable VFX module schemas and execution phases.",
        ),
        specialized(
            "vfx.inspect",
            AuthoringDomain::Vfx,
            AuthoringCapabilityKind::Query,
            vfx_documents.clone(),
            document_schema("effect"),
            AuthoringSchemaRef::of_type("VfxCompilation"),
            "Inspect one semantic VFX document and its compiled backend-neutral plan.",
        ),
        specialized(
            "vfx.validate",
            AuthoringDomain::Vfx,
            AuthoringCapabilityKind::Validation,
            vfx_documents.clone(),
            document_schema("effect"),
            AuthoringSchemaRef::of_type("VfxValidation"),
            "Validate one semantic VFX document with stable-ID diagnostics.",
        ),
        specialized(
            "vfx.preview",
            AuthoringDomain::Vfx,
            AuthoringCapabilityKind::PreviewMutation,
            vfx_documents.clone(),
            document_command_schema("effect", "VfxCommand"),
            AuthoringSchemaRef::of_type("VfxApply"),
            "Preview a VFX command transaction without committing the source document.",
        ),
        specialized(
            "vfx.apply",
            AuthoringDomain::Vfx,
            AuthoringCapabilityKind::CommittedMutation,
            vfx_documents.clone(),
            document_command_schema("effect", "VfxCommand"),
            AuthoringSchemaRef::of_type("VfxApply"),
            "Apply one atomic VFX command transaction and return the committed document plus undo commands.",
        ),
        specialized(
            "vfx.template",
            AuthoringDomain::Vfx,
            AuthoringCapabilityKind::Operation,
            vfx_documents,
            AuthoringSchemaRef::typed_json(
                "VfxTemplate",
                json!({
                    "type": "object",
                    "required": ["template"],
                    "properties": {
                        "template": {
                            "type": "string",
                            "enum": ["spark", "smoke", "burst", "trail"]
                        }
                    },
                    "additionalProperties": false
                }),
            ),
            AuthoringSchemaRef::of_type("VfxEffect"),
            "Create ordinary VFX document data from a built-in starting template.",
        ),
        specialized(
            "behavior_tree.schemas",
            AuthoringDomain::BehaviorTree,
            AuthoringCapabilityKind::Query,
            graph_documents(),
            AuthoringSchemaRef::empty(),
            AuthoringSchemaRef::of_type("BehaviorTreeSchemaCatalog"),
            "Discover Behavior Tree graph kind, layout policy, node schemas, ports, and properties.",
        ),
        specialized(
            "behavior_tree.validate",
            AuthoringDomain::BehaviorTree,
            AuthoringCapabilityKind::Validation,
            graph_documents(),
            document_schema("graph"),
            AuthoringSchemaRef::of_type("BehaviorTreeValidation"),
            "Validate a Behavior Tree semantic graph.",
        ),
        specialized(
            "behavior_tree.compile",
            AuthoringDomain::BehaviorTree,
            AuthoringCapabilityKind::Operation,
            graph_documents(),
            document_schema("graph"),
            AuthoringSchemaRef::of_type("BehaviorTreeCompilation"),
            "Compile a Behavior Tree semantic graph into a runtime tree artifact.",
        ),
        specialized(
            "behavior_tree.layout",
            AuthoringDomain::BehaviorTree,
            AuthoringCapabilityKind::Operation,
            graph_documents(),
            document_schema("graph"),
            AuthoringSchemaRef::of_type("BehaviorTreeLayout"),
            "Generate a deterministic top-down graph view for a Behavior Tree graph.",
        ),
        specialized(
            "behavior_tree.nodes",
            AuthoringDomain::BehaviorTree,
            AuthoringCapabilityKind::Query,
            graph_documents(),
            document_schema("graph"),
            AuthoringSchemaRef::of_type("BehaviorTreeNodeSummary"),
            "List Behavior Tree nodes with stable IDs, node types, names, and properties.",
        ),
        specialized(
            "behavior_tree.edges",
            AuthoringDomain::BehaviorTree,
            AuthoringCapabilityKind::Query,
            graph_documents(),
            document_schema("graph"),
            AuthoringSchemaRef::of_type("BehaviorTreeEdgeSummary"),
            "List Behavior Tree edges with stable IDs and port endpoints.",
        ),
        specialized(
            "behavior_tree.apply",
            AuthoringDomain::BehaviorTree,
            AuthoringCapabilityKind::CommittedMutation,
            graph_documents(),
            document_command_schema("graph", "GraphCommand"),
            AuthoringSchemaRef::of_type("BehaviorTreeApply"),
            "Apply a bulk Behavior Tree graph command transaction and return diff, diagnostics, and the updated graph.",
        ),
    ]
}

fn validate_capability_id(value: &str) -> Result<(), AuthoringCapabilityIdError> {
    if value.is_empty() {
        return Err(AuthoringCapabilityIdError::Empty);
    }
    if !value.contains('.') {
        return Err(AuthoringCapabilityIdError::MissingNamespace);
    }
    for segment in value.split('.') {
        if segment.is_empty() {
            return Err(AuthoringCapabilityIdError::EmptySegment);
        }
        let mut bytes = segment.bytes();
        let starts_with_lowercase = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
        let remaining_are_valid =
            bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if !starts_with_lowercase || !remaining_are_valid {
            return Err(AuthoringCapabilityIdError::InvalidSegment {
                segment: segment.to_owned(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_ids_must_be_lowercase_dotted_identifiers() {
        assert!(AuthoringCapabilityId::try_new("scene.apply").is_ok());
        assert_eq!(
            AuthoringCapabilityId::try_new("scene"),
            Err(AuthoringCapabilityIdError::MissingNamespace)
        );
        assert_eq!(
            AuthoringCapabilityId::try_new("Scene.Apply"),
            Err(AuthoringCapabilityIdError::InvalidSegment {
                segment: "Scene".to_owned()
            })
        );
        assert_eq!(
            AuthoringCapabilityId::try_new("scene..apply"),
            Err(AuthoringCapabilityIdError::EmptySegment)
        );
    }

    #[test]
    fn builtin_registry_is_deterministic_and_free_of_duplicates() {
        let first = AuthoringCapabilityRegistry::builtin();
        let second = AuthoringCapabilityRegistry::builtin();
        let ids = first
            .capabilities()
            .map(|capability| capability.id.as_str().to_owned())
            .collect::<Vec<_>>();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();

        assert_eq!(ids, sorted);
        assert_eq!(first, second);
        assert!(!first.is_empty());
    }

    #[test]
    fn generic_scene_capabilities_are_registered_first_class() {
        let registry = AuthoringCapabilityRegistry::builtin();

        for id in [
            "scene.inspect",
            "scene.validate",
            "scene.preview",
            "scene.apply",
            "entity.find",
            "entity.inspect",
            "component.schemas",
        ] {
            let capability = registry
                .require(&AuthoringCapabilityId::new(id))
                .expect("generic Scene capability must be registered");
            assert!(capability.is_generic(), "{id} must be generic");
        }
    }

    #[test]
    fn capability_kind_agrees_with_permission_and_transaction_contract() {
        let registry = AuthoringCapabilityRegistry::builtin();

        for capability in registry.capabilities() {
            match capability.kind {
                AuthoringCapabilityKind::Query
                | AuthoringCapabilityKind::Validation
                | AuthoringCapabilityKind::Operation => {
                    assert_eq!(
                        capability.transaction,
                        AuthoringTransactionRequirement::None,
                        "{} must not require a mutation base",
                        capability.id
                    );
                    assert_eq!(
                        capability.permission,
                        AuthoringPermission::Read,
                        "{} must be a read operation",
                        capability.id
                    );
                }
                AuthoringCapabilityKind::PreviewMutation => {
                    assert_ne!(
                        capability.transaction,
                        AuthoringTransactionRequirement::None,
                        "{} must declare a mutation base",
                        capability.id
                    );
                    assert_eq!(capability.permission, AuthoringPermission::Preview);
                }
                AuthoringCapabilityKind::CommittedMutation => {
                    assert_ne!(
                        capability.transaction,
                        AuthoringTransactionRequirement::None,
                        "{} must declare a mutation base",
                        capability.id
                    );
                    assert!(
                        matches!(
                            capability.permission,
                            AuthoringPermission::ProjectDataWrite | AuthoringPermission::AssetWrite
                        ),
                        "{} must require a write permission",
                        capability.id
                    );
                }
            }
        }
    }

    #[test]
    fn generic_mutations_reject_stale_live_document_bases() {
        let registry = AuthoringCapabilityRegistry::builtin();

        for capability in registry.capabilities().filter(|entry| entry.is_generic()) {
            match capability.kind {
                AuthoringCapabilityKind::PreviewMutation => assert_eq!(
                    capability.transaction,
                    AuthoringTransactionRequirement::RevisionChecked,
                    "generic {} must preview against an exact live base",
                    capability.id
                ),
                AuthoringCapabilityKind::CommittedMutation => assert_eq!(
                    capability.transaction,
                    AuthoringTransactionRequirement::AtomicCommit,
                    "generic {} must commit one atomic transaction",
                    capability.id
                ),
                AuthoringCapabilityKind::Query
                | AuthoringCapabilityKind::Validation
                | AuthoringCapabilityKind::Operation => {}
            }
        }
    }

    #[test]
    fn every_capability_documents_its_contract() {
        for capability in AuthoringCapabilityRegistry::builtin().capabilities() {
            assert!(
                !capability.description.trim().is_empty(),
                "{} must be described for humans and AI clients",
                capability.id
            );
            assert!(
                !capability.documents.is_empty(),
                "{} must declare the documents it uses",
                capability.id
            );
            assert!(
                capability.output.type_name.is_some(),
                "{} must reference a shared output type",
                capability.id
            );
        }
    }

    #[test]
    fn discovering_a_capability_does_not_grant_permission() {
        let registry = AuthoringCapabilityRegistry::builtin();
        let apply = registry
            .require(&AuthoringCapabilityId::new("scene.apply"))
            .expect("scene.apply must be registered");

        let error = apply
            .require_permission(&AuthoringPermissions::read_only())
            .expect_err("read-only sessions must not commit Scene mutations");

        assert_eq!(error.code(), "authoring.permission_denied");
        assert_eq!(error.required(), AuthoringPermission::ProjectDataWrite);
    }

    #[test]
    fn duplicate_capability_registration_is_rejected() {
        let mut registry = AuthoringCapabilityRegistry::builtin();
        let duplicate = query(
            "scene.inspect",
            AuthoringDomain::Scene,
            scene_documents(),
            AuthoringSchemaRef::of_type("SceneAuthoringSnapshot"),
            "duplicate",
        );

        let error = registry
            .register(duplicate)
            .expect_err("duplicate capability IDs must be rejected");

        assert_eq!(error.code(), "authoring.capability_duplicate");
    }

    #[test]
    fn reserved_generic_namespace_cannot_be_registered() {
        let mut registry = AuthoringCapabilityRegistry::empty();
        let reserved = query(
            "authoring.apply",
            AuthoringDomain::Scene,
            scene_documents(),
            AuthoringSchemaRef::of_type("SceneAuthoringMutation"),
            "reserved",
        );

        let error = registry
            .register(reserved)
            .expect_err("the generic adapter namespace must stay reserved");

        assert_eq!(error.code(), "authoring.capability_reserved_namespace");
    }

    #[test]
    fn unknown_capability_lookup_has_stable_code() {
        let registry = AuthoringCapabilityRegistry::builtin();

        let error = registry
            .require(&AuthoringCapabilityId::new("scene.does_not_exist"))
            .expect_err("unknown capability must be rejected");

        assert_eq!(error.code(), "authoring.capability_unknown");
    }

    #[test]
    fn capability_ids_round_trip_through_json() {
        let id = AuthoringCapabilityId::new("graph.layout.apply");
        let json = serde_json::to_string(&id).expect("capability ID must serialize");

        assert_eq!(json, "\"graph.layout.apply\"");
        assert_eq!(
            serde_json::from_str::<AuthoringCapabilityId>(&json).expect("valid ID"),
            id
        );
        assert!(serde_json::from_str::<AuthoringCapabilityId>("\"Bad Id\"").is_err());
    }
}
