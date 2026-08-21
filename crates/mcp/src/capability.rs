//! Registry-driven generic structured authoring surface (ADR 0132).
//!
//! These handlers own no capability catalog of their own. Tool names,
//! descriptions, argument schemas, and permission requirements all come from
//! the canonical [`engine_authoring::AuthoringCapabilityRegistry`],
//! so a capability cannot exist for the Editor while silently missing from the
//! structured AI surface.
//!
//! The adapter binds exactly one MCP tool name per capability ID. Generic
//! invocation therefore resolves a capability to the tool that already routes
//! it into the live authoring host, and the host executes that tool through the
//! same shared service, permission, and transaction path a human edit uses.

use crate::McpToolDescriptor;
use engine_authoring::{
    AuthoringCapability, AuthoringCapabilityError, AuthoringCapabilityExposure,
    AuthoringCapabilityId, AuthoringCapabilityIdError, AuthoringCapabilityKind,
    AuthoringCapabilityRegistry, AuthoringCapabilitySummary, AuthoringDomain, AuthoringPermission,
    AuthoringPermissionError, AuthoringPermissions,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;

/// Tool name for context-efficient registry-driven capability discovery.
pub const AUTHORING_LIST_TOOL: &str = "authoring.list";
/// Tool name for full registry-driven capability discovery retained for compatibility.
pub const AUTHORING_CAPABILITIES_TOOL: &str = "authoring.capabilities";
/// Tool name for single-capability metadata lookup.
pub const AUTHORING_DESCRIBE_TOOL: &str = "authoring.describe";
/// Tool name for generic capability inspection.
pub const AUTHORING_INSPECT_TOOL: &str = "authoring.inspect";
/// Tool name for generic capability validation.
pub const AUTHORING_VALIDATE_TOOL: &str = "authoring.validate";
/// Tool name for generic capability preview.
pub const AUTHORING_PREVIEW_TOOL: &str = "authoring.preview";
/// Tool name for generic capability apply.
pub const AUTHORING_APPLY_TOOL: &str = "authoring.apply";

/// Generic operation requested through the registry-driven surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoringVerb {
    /// Read authoring state.
    Inspect,
    /// Validate authoring state.
    Validate,
    /// Compute a mutation without committing it.
    Preview,
    /// Commit one atomic mutation.
    Apply,
}

impl AuthoringVerb {
    /// Returns the capability kind this verb may invoke.
    pub fn kind(self) -> AuthoringCapabilityKind {
        match self {
            Self::Inspect => AuthoringCapabilityKind::Query,
            Self::Validate => AuthoringCapabilityKind::Validation,
            Self::Preview => AuthoringCapabilityKind::PreviewMutation,
            Self::Apply => AuthoringCapabilityKind::CommittedMutation,
        }
    }

    /// Returns the MCP tool name that exposes this verb.
    pub fn tool_name(self) -> &'static str {
        match self {
            Self::Inspect => AUTHORING_INSPECT_TOOL,
            Self::Validate => AUTHORING_VALIDATE_TOOL,
            Self::Preview => AUTHORING_PREVIEW_TOOL,
            Self::Apply => AUTHORING_APPLY_TOOL,
        }
    }

    /// Returns every generic verb in stable order.
    pub fn all() -> [Self; 4] {
        [Self::Inspect, Self::Validate, Self::Preview, Self::Apply]
    }
}

/// Input for `authoring.describe`.
#[derive(Debug, Clone, Deserialize)]
pub struct CapabilityDescribeInput {
    /// Capability to describe.
    pub capability: AuthoringCapabilityId,
}

/// Input shared by the generic inspect, validate, preview, and apply tools.
#[derive(Debug, Clone, Deserialize)]
pub struct CapabilityInvokeInput {
    /// Capability to invoke.
    pub capability: AuthoringCapabilityId,
    /// Arguments matching the capability input schema.
    #[serde(default = "empty_arguments")]
    pub arguments: Value,
}

fn empty_arguments() -> Value {
    json!({})
}

/// Output for context-efficient `authoring.list` discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilitySummaryListOutput {
    /// Registry-derived capability summaries in deterministic capability-ID order.
    pub capabilities: Vec<AuthoringCapabilitySummary>,
}

/// Full output for `authoring.capabilities`, retained for compatibility.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CapabilityListOutput {
    /// Registered full capability descriptors in deterministic capability-ID order.
    pub capabilities: Vec<AuthoringCapability>,
}

/// Output for `authoring.describe`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CapabilityDescribeOutput {
    /// Canonical registry metadata.
    pub capability: AuthoringCapability,
    /// MCP tool that executes this capability against the live host.
    pub tool: String,
}

/// Resolved generic invocation ready to run against the live authoring host.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthoringInvocationPlan {
    /// Capability that was resolved.
    pub capability: AuthoringCapabilityId,
    /// MCP tool that carries the capability into the live host.
    pub tool: String,
    /// Arguments forwarded to that tool unchanged.
    pub arguments: Value,
}

/// Reports why the generic authoring surface rejected a request.
#[derive(Debug, Clone, PartialEq)]
pub enum CapabilityMcpError {
    /// The registry rejected the capability lookup.
    Registry(AuthoringCapabilityError),
    /// The session does not hold the permission the capability requires.
    Permission(AuthoringPermissionError),
    /// The capability exists but does not answer the requested generic verb.
    VerbMismatch {
        /// Requested capability.
        capability: AuthoringCapabilityId,
        /// Requested generic verb.
        verb: AuthoringVerb,
        /// Kind the capability actually declares.
        kind: AuthoringCapabilityKind,
    },
    /// The capability is not exposed through the generic surface.
    NotGeneric {
        /// Requested capability.
        capability: AuthoringCapabilityId,
        /// Exposure the registry declares for it.
        exposure: AuthoringCapabilityExposure,
    },
    /// The requested capability name is not a valid capability identifier.
    InvalidCapabilityId(AuthoringCapabilityIdError),
}

impl CapabilityMcpError {
    /// Returns the stable diagnostic-style code exposed to MCP clients.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Registry(error) => error.code(),
            Self::Permission(error) => error.code(),
            Self::VerbMismatch { .. } => "mcp.capability_verb_mismatch",
            Self::NotGeneric { .. } => "mcp.capability_not_generic",
            Self::InvalidCapabilityId(_) => "mcp.capability_invalid_id",
        }
    }
}

impl fmt::Display for CapabilityMcpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry(error) => error.fmt(formatter),
            Self::Permission(error) => error.fmt(formatter),
            Self::VerbMismatch {
                capability,
                verb,
                kind,
            } => write!(
                formatter,
                "capability `{capability}` is a {kind:?} operation and cannot answer `{}`",
                verb.tool_name()
            ),
            Self::NotGeneric {
                capability,
                exposure,
            } => write!(
                formatter,
                "capability `{capability}` requires its specialized adapter operation ({exposure:?})"
            ),
            Self::InvalidCapabilityId(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CapabilityMcpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Registry(error) => Some(error),
            Self::Permission(error) => Some(error),
            Self::InvalidCapabilityId(error) => Some(error),
            Self::VerbMismatch { .. } | Self::NotGeneric { .. } => None,
        }
    }
}

impl From<AuthoringCapabilityError> for CapabilityMcpError {
    fn from(error: AuthoringCapabilityError) -> Self {
        Self::Registry(error)
    }
}

impl From<AuthoringPermissionError> for CapabilityMcpError {
    fn from(error: AuthoringPermissionError) -> Self {
        Self::Permission(error)
    }
}

/// Generic registry-driven authoring tools.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthoringCapabilityMcpTools {
    registry: AuthoringCapabilityRegistry,
}

impl AuthoringCapabilityMcpTools {
    /// Creates the generic surface over the built-in capability registry.
    pub fn new() -> Self {
        Self::with_registry(AuthoringCapabilityRegistry::builtin())
    }

    /// Creates the generic surface over a host-supplied registry.
    pub fn with_registry(registry: AuthoringCapabilityRegistry) -> Self {
        Self { registry }
    }

    /// Returns the capability registry backing this surface.
    pub fn registry(&self) -> &AuthoringCapabilityRegistry {
        &self.registry
    }

    /// Returns the MCP tool that executes `capability`.
    ///
    /// The adapter binds one tool name per capability ID so structured clients
    /// can move between the generic surface and domain-specific tools without
    /// consulting a second catalog.
    pub fn tool_name(capability: &AuthoringCapability) -> &str {
        capability.id.as_str()
    }

    /// Returns descriptors for the generic authoring surface.
    ///
    /// Every descriptor is derived from the registry, including the capability
    /// enumerations advertised for each generic verb.
    pub fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        let mut descriptors = vec![
            descriptor(
                AUTHORING_LIST_TOOL,
                "List registered authoring capabilities using compact registry summaries. Use authoring.describe for the selected capability's schema, permission, transaction, and document contract.",
                empty_object_schema(),
            ),
            descriptor(
                AUTHORING_CAPABILITIES_TOOL,
                "Return every full registered capability descriptor for compatibility. Prefer authoring.list followed by authoring.describe for context-efficient discovery.",
                empty_object_schema(),
            ),
            descriptor(
                AUTHORING_DESCRIBE_TOOL,
                "Describe one registered authoring capability and the tool that executes it.",
                json!({
                    "type": "object",
                    "required": ["capability"],
                    "properties": {
                        "capability": {"type": "string", "enum": self.capability_ids(None)}
                    },
                    "additionalProperties": false
                }),
            ),
        ];
        for verb in AuthoringVerb::all() {
            descriptors.push(descriptor(
                verb.tool_name(),
                verb_description(verb),
                self.invoke_schema(verb),
            ));
        }
        descriptors
    }

    /// Lists compact summaries for every registered capability.
    ///
    /// The summary projection comes directly from the canonical registry and
    /// intentionally omits the detail fields available from [`Self::describe`].
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityMcpError`] when read permission is absent.
    pub fn list(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<CapabilitySummaryListOutput, CapabilityMcpError> {
        permissions.require(AuthoringPermission::Read)?;
        Ok(CapabilitySummaryListOutput {
            capabilities: self.registry.summaries().collect(),
        })
    }

    /// Lists every full registered capability for compatibility.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityMcpError`] when read permission is absent.
    pub fn capabilities(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<CapabilityListOutput, CapabilityMcpError> {
        permissions.require(AuthoringPermission::Read)?;
        Ok(CapabilityListOutput {
            capabilities: self.registry.capabilities().cloned().collect(),
        })
    }

    /// Describes one registered capability.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityMcpError`] when read permission is absent or the
    /// capability is not registered.
    pub fn describe(
        &self,
        permissions: &AuthoringPermissions,
        input: CapabilityDescribeInput,
    ) -> Result<CapabilityDescribeOutput, CapabilityMcpError> {
        permissions.require(AuthoringPermission::Read)?;
        let capability = self.registry.require(&input.capability)?;
        Ok(CapabilityDescribeOutput {
            tool: Self::tool_name(capability).to_owned(),
            capability: capability.clone(),
        })
    }

    /// Resolves one generic invocation into the tool that executes it.
    ///
    /// Permission is checked here so discovery cannot be mistaken for
    /// authorization; the shared authoring service still enforces the same
    /// permission, validation, and stale-revision rules when the tool runs.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityMcpError`] when the capability is unknown, is not
    /// exposed generically, does not answer the requested verb, or the session
    /// lacks the required permission.
    pub fn plan(
        &self,
        verb: AuthoringVerb,
        permissions: &AuthoringPermissions,
        input: CapabilityInvokeInput,
    ) -> Result<AuthoringInvocationPlan, CapabilityMcpError> {
        let capability = self.registry.require(&input.capability)?;
        if !capability.is_generic() {
            return Err(CapabilityMcpError::NotGeneric {
                capability: capability.id.clone(),
                exposure: capability.exposure.clone(),
            });
        }
        if capability.kind != verb.kind() {
            return Err(CapabilityMcpError::VerbMismatch {
                capability: capability.id.clone(),
                verb,
                kind: capability.kind,
            });
        }
        capability.require_permission(permissions)?;
        Ok(AuthoringInvocationPlan {
            capability: capability.id.clone(),
            tool: Self::tool_name(capability).to_owned(),
            arguments: input.arguments,
        })
    }

    fn capability_ids(&self, kind: Option<AuthoringCapabilityKind>) -> Vec<String> {
        self.registry
            .capabilities()
            .filter(|capability| match kind {
                Some(kind) => capability.is_generic() && capability.kind == kind,
                None => true,
            })
            .map(|capability| capability.id.as_str().to_owned())
            .collect()
    }

    fn invoke_schema(&self, verb: AuthoringVerb) -> Value {
        json!({
            "type": "object",
            "required": ["capability"],
            "properties": {
                "capability": {"type": "string", "enum": self.capability_ids(Some(verb.kind()))},
                "arguments": {"type": "object"}
            },
            "additionalProperties": false
        })
    }
}

impl Default for AuthoringCapabilityMcpTools {
    fn default() -> Self {
        Self::new()
    }
}

/// One capability and the MCP tool that covers it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CapabilityCoverage {
    /// Registered capability.
    pub capability: AuthoringCapabilityId,
    /// Declared exposure for the capability.
    pub exposure: AuthoringCapabilityExposure,
    /// MCP tool that covers it, when one is advertised.
    pub tool: Option<String>,
}

/// Reports how each registered capability is covered by an MCP tool.
///
/// ADR 0132 requires parity guardrails to iterate the canonical registry
/// instead of comparing independently maintained adapter lists, so this is the
/// function tests use to prove coverage.
pub fn capability_coverage(
    registry: &AuthoringCapabilityRegistry,
    descriptors: &[McpToolDescriptor],
) -> Vec<CapabilityCoverage> {
    registry
        .capabilities()
        .map(|capability| CapabilityCoverage {
            tool: descriptors
                .iter()
                .find(|descriptor| descriptor.name == capability.id.as_str())
                .map(|descriptor| descriptor.name.clone()),
            capability: capability.id.clone(),
            exposure: capability.exposure.clone(),
        })
        .collect()
}

/// Returns capabilities that no advertised MCP tool exposes.
pub fn uncovered_capabilities(
    registry: &AuthoringCapabilityRegistry,
    descriptors: &[McpToolDescriptor],
) -> Vec<AuthoringCapabilityId> {
    capability_coverage(registry, descriptors)
        .into_iter()
        .filter(|coverage| coverage.tool.is_none())
        .map(|coverage| coverage.capability)
        .collect()
}

/// Requires the permission the registry declares for one capability.
///
/// Specialized adapters whose shared service does not itself receive the
/// session permission set call this, so the enforced permission is the registry
/// contract instead of a constant retyped in adapter code (ADR 0132 §5, §6).
///
/// # Errors
///
/// Returns [`CapabilityMcpError`] when `id` is malformed, is not registered, or
/// the session lacks the declared permission.
pub fn authorize_capability(
    registry: &AuthoringCapabilityRegistry,
    id: &str,
    permissions: &AuthoringPermissions,
) -> Result<(), CapabilityMcpError> {
    let id = AuthoringCapabilityId::try_new(id).map_err(CapabilityMcpError::InvalidCapabilityId)?;
    registry.authorize(&id, permissions)?;
    Ok(())
}

/// Returns registry-derived tool descriptors for one or more authoring domains.
///
/// Domain adapters call this instead of hand-writing tool names, descriptions,
/// and argument schemas, so their advertised surface cannot disagree with the
/// canonical registry.
pub fn domain_tool_descriptors(
    registry: &AuthoringCapabilityRegistry,
    domains: &[AuthoringDomain],
) -> Vec<McpToolDescriptor> {
    domains
        .iter()
        .flat_map(|domain| registry.domain(*domain))
        .map(|capability| McpToolDescriptor {
            name: AuthoringCapabilityMcpTools::tool_name(capability).to_owned(),
            description: capability.description.clone(),
            input_schema: capability.input.json_schema.clone(),
        })
        .collect()
}

fn verb_description(verb: AuthoringVerb) -> &'static str {
    match verb {
        AuthoringVerb::Inspect => {
            "Run one registered read capability through the generic authoring surface."
        }
        AuthoringVerb::Validate => {
            "Run one registered validation capability through the generic authoring surface."
        }
        AuthoringVerb::Preview => {
            "Preview one registered mutation capability against an exact document base."
        }
        AuthoringVerb::Apply => {
            "Apply one registered mutation capability as a single authoring transaction."
        }
    }
}

fn descriptor(name: &str, description: &str, input_schema: Value) -> McpToolDescriptor {
    McpToolDescriptor {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
    }
}

fn empty_object_schema() -> Value {
    json!({"type": "object", "properties": {}, "additionalProperties": false})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring_tool_descriptors;

    fn writable() -> AuthoringPermissions {
        AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite)
            .with(AuthoringPermission::AssetWrite)
    }

    #[test]
    fn generic_surface_advertises_compact_discovery_and_the_adr_operations() {
        let names = AuthoringCapabilityMcpTools::new()
            .tool_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "authoring.list",
                "authoring.capabilities",
                "authoring.describe",
                "authoring.inspect",
                "authoring.validate",
                "authoring.preview",
                "authoring.apply",
            ]
        );
    }

    #[test]
    fn generic_tool_schemas_are_derived_from_the_registry() {
        let tools = AuthoringCapabilityMcpTools::new();
        let descriptors = tools.tool_descriptors();
        let apply = descriptors
            .iter()
            .find(|descriptor| descriptor.name == AUTHORING_APPLY_TOOL)
            .expect("apply tool must be advertised");
        let advertised = apply.input_schema["properties"]["capability"]["enum"]
            .as_array()
            .expect("capability enumeration")
            .iter()
            .map(|value| value.as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        let expected = tools
            .registry()
            .capabilities()
            .filter(|capability| {
                capability.is_generic()
                    && capability.kind == AuthoringCapabilityKind::CommittedMutation
            })
            .map(|capability| capability.id.as_str().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(advertised, expected);
        assert!(advertised.contains(&"scene.apply".to_owned()));
        assert!(!advertised.contains(&"scene.inspect".to_owned()));
    }

    #[test]
    fn compact_discovery_is_registry_derived_and_materially_smaller() {
        let tools = AuthoringCapabilityMcpTools::new();
        let permissions = AuthoringPermissions::read_only();
        let compact = tools.list(&permissions).expect("compact discovery");
        let full = tools
            .capabilities(&permissions)
            .expect("full compatibility discovery");
        let expected = tools.registry().summaries().collect::<Vec<_>>();

        assert_eq!(compact.capabilities, expected);
        assert_eq!(
            compact
                .capabilities
                .iter()
                .map(|capability| capability.id.clone())
                .collect::<Vec<_>>(),
            full.capabilities
                .iter()
                .map(|capability| capability.id.clone())
                .collect::<Vec<_>>()
        );
        for (summary, capability) in compact.capabilities.iter().zip(&full.capabilities) {
            assert_eq!(summary.exposure, capability.exposure);
        }

        let compact_bytes = serde_json::to_vec(&compact)
            .expect("compact discovery must serialize")
            .len();
        let full_bytes = serde_json::to_vec(&full)
            .expect("full discovery must serialize")
            .len();
        assert!(
            compact_bytes * 2 <= full_bytes,
            "compact discovery must be at least 50% smaller: compact={compact_bytes} full={full_bytes}"
        );
    }

    #[test]
    fn describe_returns_the_canonical_full_schema_contract() {
        let tools = AuthoringCapabilityMcpTools::new();
        let id = AuthoringCapabilityId::new("scene.apply");
        let expected = tools
            .registry()
            .require(&id)
            .expect("scene.apply registry entry");
        let described = tools
            .describe(
                &AuthoringPermissions::read_only(),
                CapabilityDescribeInput { capability: id },
            )
            .expect("detail discovery");

        assert_eq!(described.capability, *expected);
        assert_eq!(described.tool, "scene.apply");
    }

    #[test]
    fn every_registered_capability_is_covered_by_an_adapter_tool() {
        let registry = AuthoringCapabilityRegistry::builtin();
        let descriptors = authoring_tool_descriptors();

        let uncovered = uncovered_capabilities(&registry, &descriptors)
            .into_iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>();

        assert!(
            uncovered.is_empty(),
            "capabilities without an MCP tool: {uncovered:?}"
        );
    }

    #[test]
    fn every_advertised_tool_is_derived_from_the_registry() {
        let registry = AuthoringCapabilityRegistry::builtin();
        let generic = AuthoringCapabilityMcpTools::new()
            .tool_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();

        for descriptor in authoring_tool_descriptors() {
            if generic.contains(&descriptor.name) {
                continue;
            }
            let capability = registry
                .require(&AuthoringCapabilityId::new(descriptor.name.clone()))
                .unwrap_or_else(|_| {
                    panic!(
                        "advertised tool `{}` is not a registered capability",
                        descriptor.name
                    )
                });
            assert_eq!(
                descriptor.description, capability.description,
                "`{}` description must come from the registry",
                descriptor.name
            );
            assert_eq!(
                descriptor.input_schema, capability.input.json_schema,
                "`{}` argument schema must come from the registry",
                descriptor.name
            );
        }
    }

    #[test]
    fn specialized_adapters_enforce_registry_declared_permissions() {
        let registry = AuthoringCapabilityRegistry::builtin();
        let read_only = AuthoringPermissions::read_only();

        for capability in registry
            .capabilities()
            .filter(|capability| !capability.is_generic())
        {
            let authorized =
                authorize_capability(&registry, capability.id.as_str(), &read_only).is_ok();
            assert_eq!(
                authorized,
                capability.permission == AuthoringPermission::Read,
                "`{}` authorization must follow its declared permission",
                capability.id
            );
        }
    }

    #[test]
    fn generic_capabilities_resolve_to_their_live_host_tool() {
        let tools = AuthoringCapabilityMcpTools::new();

        let plan = tools
            .plan(
                AuthoringVerb::Apply,
                &writable(),
                CapabilityInvokeInput {
                    capability: AuthoringCapabilityId::new("scene.apply"),
                    arguments: json!({
                        "expected_revision": 0,
                        "expected_generation": 0,
                        "commands": []
                    }),
                },
            )
            .expect("scene.apply must resolve");

        assert_eq!(plan.tool, "scene.apply");
        assert_eq!(plan.arguments["expected_revision"], json!(0));
    }

    #[test]
    fn verb_and_capability_kind_must_agree() {
        let error = AuthoringCapabilityMcpTools::new()
            .plan(
                AuthoringVerb::Apply,
                &writable(),
                CapabilityInvokeInput {
                    capability: AuthoringCapabilityId::new("scene.inspect"),
                    arguments: json!({}),
                },
            )
            .expect_err("a query capability cannot be applied");

        assert_eq!(error.code(), "mcp.capability_verb_mismatch");
    }

    #[test]
    fn specialized_capabilities_are_not_callable_generically() {
        let error = AuthoringCapabilityMcpTools::new()
            .plan(
                AuthoringVerb::Apply,
                &writable(),
                CapabilityInvokeInput {
                    capability: AuthoringCapabilityId::new("behavior_tree.apply"),
                    arguments: json!({}),
                },
            )
            .expect_err("specialized capabilities need their declared adapter path");

        assert_eq!(error.code(), "mcp.capability_not_generic");
    }

    #[test]
    fn discovery_does_not_weaken_shared_authorization() {
        let tools = AuthoringCapabilityMcpTools::new();
        let read_only = AuthoringPermissions::read_only();

        let listed = tools
            .list(&read_only)
            .expect("read-only sessions may discover compact capability summaries");
        assert!(
            listed
                .capabilities
                .iter()
                .any(|capability| capability.id.as_str() == "scene.apply")
        );
        let described = tools
            .describe(
                &read_only,
                CapabilityDescribeInput {
                    capability: AuthoringCapabilityId::new("scene.apply"),
                },
            )
            .expect("read-only sessions may still discover mutations");
        let error = tools
            .plan(
                AuthoringVerb::Apply,
                &read_only,
                CapabilityInvokeInput {
                    capability: AuthoringCapabilityId::new("scene.apply"),
                    arguments: json!({}),
                },
            )
            .expect_err("read-only sessions must not commit");

        assert_eq!(described.tool, "scene.apply");
        assert_eq!(error.code(), "authoring.permission_denied");
    }

    #[test]
    fn unknown_capability_is_rejected_with_the_registry_code() {
        let error = AuthoringCapabilityMcpTools::new()
            .describe(
                &AuthoringPermissions::read_only(),
                CapabilityDescribeInput {
                    capability: AuthoringCapabilityId::new("scene.does_not_exist"),
                },
            )
            .expect_err("unknown capabilities must be rejected");

        assert_eq!(error.code(), "authoring.capability_unknown");
    }

    #[test]
    fn capability_listing_requires_read_permission() {
        let error = AuthoringCapabilityMcpTools::new()
            .capabilities(&AuthoringPermissions::none())
            .expect_err("capability discovery requires read permission");

        assert_eq!(error.code(), "authoring.permission_denied");
    }
}
