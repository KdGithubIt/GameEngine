//! Process-local routing from stable AI Studio selections to execution drivers.
//!
//! Logical AI identities belong to AI Studio and Remote AI Studio. ACP agent IDs
//! belong to the runtime registry. This layer joins them without serializing
//! driver choice into preferences or silently falling back after an ACP route
//! has been selected.

use crate::acp_agent_runtime::AcpAgentRegistry;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Execution driver selected for one logical AI target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AiExecutionDriver {
    /// Existing provider/native execution remains responsible for this target.
    Legacy,
    /// Execution must use the named ACP registry entry.
    Acp {
        /// Descriptor ID registered at the ACP runtime boundary.
        agent_id: String,
    },
}

/// Resolution of one stable logical selection into its process-local driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AiExecutionResolution {
    /// Stable AI Studio identity, including the managed model identity when relevant.
    pub(crate) logical_ai_id: String,
    /// Stable routing family used to share one driver across managed-model identities.
    pub(crate) route_key: String,
    /// Internal execution driver. This is never used as the user-visible AI identity.
    pub(crate) driver: AiExecutionDriver,
}

/// Process-local driver table plus the ACP descriptors currently registered.
///
/// An absent route deliberately means the target has not migrated yet and uses
/// the legacy executor. Once an explicit ACP route exists, an unavailable ACP
/// descriptor is an error; resolution never converts that failure into Legacy.
#[derive(Debug, Default)]
pub(super) struct AiExecutionRouter {
    routes: BTreeMap<String, AiExecutionDriver>,
    available_acp_agents: BTreeSet<String>,
}

impl AiExecutionRouter {
    /// Routes one logical selection family through a concrete ACP registry ID.
    pub(super) fn set_acp_route(
        &mut self,
        route_key: impl Into<String>,
        agent_id: impl Into<String>,
    ) -> Result<(), AiExecutionRoutingError> {
        let route_key = validate_route_key(route_key.into())?;
        let agent_id = validate_agent_id(agent_id.into())?;
        self.routes
            .insert(route_key, AiExecutionDriver::Acp { agent_id });
        Ok(())
    }

    /// Explicitly rolls one logical selection family back to the legacy executor.
    pub(super) fn set_legacy_route(
        &mut self,
        route_key: impl Into<String>,
    ) -> Result<(), AiExecutionRoutingError> {
        let route_key = validate_route_key(route_key.into())?;
        self.routes.insert(route_key, AiExecutionDriver::Legacy);
        Ok(())
    }

    /// Refreshes only descriptor availability from the runtime registry.
    ///
    /// The router never owns or opens ACP sessions. Final execution integration
    /// resolves the returned agent ID against the authoritative registry.
    pub(super) fn sync_registry(&mut self, registry: &dyn AcpAgentRegistry) {
        self.available_acp_agents = registry
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.id.clone())
            .collect();
    }

    /// Resolves one logical identity without changing that identity.
    pub(super) fn resolve(
        &self,
        logical_ai_id: impl Into<String>,
        route_key: impl Into<String>,
    ) -> Result<AiExecutionResolution, AiExecutionRoutingError> {
        let logical_ai_id = validate_logical_ai_id(logical_ai_id.into())?;
        let route_key = validate_route_key(route_key.into())?;
        let driver = self
            .routes
            .get(&route_key)
            .cloned()
            .unwrap_or(AiExecutionDriver::Legacy);
        if let AiExecutionDriver::Acp { agent_id } = &driver
            && !self.available_acp_agents.contains(agent_id)
        {
            return Err(AiExecutionRoutingError::AcpAgentUnavailable {
                logical_ai_id,
                agent_id: agent_id.clone(),
            });
        }
        Ok(AiExecutionResolution {
            logical_ai_id,
            route_key,
            driver,
        })
    }
}

/// Configuration or availability failure at the selection/driver boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AiExecutionRoutingError {
    /// A stable logical selection identity was empty or malformed.
    InvalidLogicalAiId(String),
    /// A route key was empty, padded, or contained whitespace.
    InvalidRouteKey(String),
    /// An ACP registry ID was empty, padded, or contained whitespace.
    InvalidAcpAgentId(String),
    /// A target is explicitly routed to ACP but that descriptor is not registered.
    AcpAgentUnavailable {
        logical_ai_id: String,
        agent_id: String,
    },
}

impl fmt::Display for AiExecutionRoutingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLogicalAiId(id) => {
                write!(formatter, "invalid logical AI identity `{id}`")
            }
            Self::InvalidRouteKey(key) => {
                write!(formatter, "invalid AI execution route key `{key}`")
            }
            Self::InvalidAcpAgentId(id) => write!(formatter, "invalid ACP agent ID `{id}`"),
            Self::AcpAgentUnavailable {
                logical_ai_id,
                agent_id,
            } => write!(
                formatter,
                "ACP execution for `{logical_ai_id}` requires registered agent `{agent_id}`, but that ACP adapter is unavailable. Register the ACP adapter or explicitly select the Legacy route; GameEngine will not silently fall back."
            ),
        }
    }
}

fn validate_logical_ai_id(value: String) -> Result<String, AiExecutionRoutingError> {
    if valid_identifier(&value) {
        Ok(value)
    } else {
        Err(AiExecutionRoutingError::InvalidLogicalAiId(value))
    }
}

fn validate_route_key(value: String) -> Result<String, AiExecutionRoutingError> {
    if valid_identifier(&value) {
        Ok(value)
    } else {
        Err(AiExecutionRoutingError::InvalidRouteKey(value))
    }
}

fn validate_agent_id(value: String) -> Result<String, AiExecutionRoutingError> {
    if valid_identifier(&value) {
        Ok(value)
    } else {
        Err(AiExecutionRoutingError::InvalidAcpAgentId(value))
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp_agent_runtime::{
        AcpAgentDescriptor, AcpAgentRuntime, AcpCapabilities, AcpRuntimeIdentity,
    };
    use std::ffi::OsString;

    struct DescriptorRegistry {
        descriptors: Vec<AcpAgentDescriptor>,
    }

    impl AcpAgentRegistry for DescriptorRegistry {
        fn descriptors(&self) -> Vec<&AcpAgentDescriptor> {
            self.descriptors.iter().collect()
        }

        fn runtime_mut(&mut self, _id: &str) -> Option<&mut (dyn AcpAgentRuntime + '_)> {
            None
        }
    }

    fn descriptor(id: &str) -> AcpAgentDescriptor {
        AcpAgentDescriptor {
            id: id.to_owned(),
            executable: OsString::from("future-acp-agent"),
            arguments: Vec::new(),
            capabilities: AcpCapabilities::default(),
            runtime_identity: AcpRuntimeIdentity::stable("future-acp-agent", None),
        }
    }

    #[test]
    fn unmigrated_targets_keep_the_legacy_driver() {
        let router = AiExecutionRouter::default();
        let resolution = router
            .resolve("agent:codex", "agent:codex")
            .expect("unmigrated route");
        assert_eq!(resolution.logical_ai_id, "agent:codex");
        assert_eq!(resolution.driver, AiExecutionDriver::Legacy);
    }

    #[test]
    fn configured_acp_route_resolves_registered_descriptor_id() {
        let mut router = AiExecutionRouter::default();
        router
            .set_acp_route("agent:codex", "acp.codex.adapter")
            .expect("configure ACP route");
        let registry = DescriptorRegistry {
            descriptors: vec![descriptor("acp.codex.adapter")],
        };
        router.sync_registry(&registry);

        let resolution = router
            .resolve("agent:codex", "agent:codex")
            .expect("registered route");
        assert_eq!(resolution.logical_ai_id, "agent:codex");
        assert_eq!(resolution.route_key, "agent:codex");
        assert_eq!(
            resolution.driver,
            AiExecutionDriver::Acp {
                agent_id: "acp.codex.adapter".to_owned(),
            }
        );
    }

    #[test]
    fn managed_model_identity_can_share_one_goose_route() {
        let mut router = AiExecutionRouter::default();
        router
            .set_acp_route("model:managed_local", "acp.goose.adapter")
            .expect("configure managed ACP route");
        let registry = DescriptorRegistry {
            descriptors: vec![descriptor("acp.goose.adapter")],
        };
        router.sync_registry(&registry);

        let resolution = router
            .resolve(
                "model:managed_local:sha256-model-a",
                "model:managed_local",
            )
            .expect("registered managed route");
        assert_eq!(
            resolution.logical_ai_id,
            "model:managed_local:sha256-model-a"
        );
        assert_eq!(
            resolution.driver,
            AiExecutionDriver::Acp {
                agent_id: "acp.goose.adapter".to_owned(),
            }
        );
    }

    #[test]
    fn unavailable_acp_route_fails_closed_instead_of_falling_back() {
        let mut router = AiExecutionRouter::default();
        router
            .set_acp_route("agent:claude-code", "acp.claude.adapter")
            .expect("configure ACP route");

        let error = router
            .resolve("agent:claude-code", "agent:claude-code")
            .expect_err("missing ACP descriptor must fail");
        assert_eq!(
            error,
            AiExecutionRoutingError::AcpAgentUnavailable {
                logical_ai_id: "agent:claude-code".to_owned(),
                agent_id: "acp.claude.adapter".to_owned(),
            }
        );
    }

    #[test]
    fn legacy_route_is_an_explicit_rollback_boundary() {
        let mut router = AiExecutionRouter::default();
        router
            .set_acp_route("agent:codex", "acp.codex.adapter")
            .expect("configure ACP route");
        router
            .set_legacy_route("agent:codex")
            .expect("select Legacy rollback");

        let resolution = router
            .resolve("agent:codex", "agent:codex")
            .expect("explicit legacy route");
        assert_eq!(resolution.driver, AiExecutionDriver::Legacy);
    }
}
