//! Process-local routing from stable AI Studio selections to execution drivers.
//!
//! User-visible logical AI identities remain stable while this table chooses
//! Legacy or one registered ACP descriptor. Driver choice is deliberately not
//! serialized into preferences.

use crate::acp_agent_runtime::AcpAgentRegistry;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AiExecutionDriver {
    Legacy,
    Acp { agent_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AiExecutionResolution {
    pub(crate) logical_ai_id: String,
    pub(crate) route_key: String,
    pub(crate) driver: AiExecutionDriver,
}

#[derive(Debug, Default)]
pub(super) struct AiExecutionRouter {
    routes: BTreeMap<String, AiExecutionDriver>,
    available_acp_agents: BTreeSet<String>,
}

impl AiExecutionRouter {
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

    pub(super) fn set_legacy_route(
        &mut self,
        route_key: impl Into<String>,
    ) -> Result<(), AiExecutionRoutingError> {
        let route_key = validate_route_key(route_key.into())?;
        self.routes.insert(route_key, AiExecutionDriver::Legacy);
        Ok(())
    }

    pub(super) fn sync_registry(&mut self, registry: &dyn AcpAgentRegistry) {
        self.available_acp_agents = registry
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.id.clone())
            .collect();
    }

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AiExecutionRoutingError {
    InvalidLogicalAiId(String),
    InvalidRouteKey(String),
    InvalidAcpAgentId(String),
    AcpAgentUnavailable {
        logical_ai_id: String,
        agent_id: String,
    },
}

impl fmt::Display for AiExecutionRoutingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLogicalAiId(id) => write!(formatter, "invalid logical AI identity `{id}`"),
            Self::InvalidRouteKey(key) => write!(formatter, "invalid AI execution route key `{key}`"),
            Self::InvalidAcpAgentId(id) => write!(formatter, "invalid ACP agent ID `{id}`"),
            Self::AcpAgentUnavailable { logical_ai_id, agent_id } => write!(
                formatter,
                "ACP execution for `{logical_ai_id}` requires registered agent `{agent_id}`, but that ACP adapter is unavailable. Register it or explicitly select the Legacy route; GameEngine will not silently fall back."
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
            environment: BTreeMap::new(),
            capabilities: AcpCapabilities::default(),
            runtime_identity: AcpRuntimeIdentity::stable("future-acp-agent", None),
        }
    }

    #[test]
    fn explicit_acp_route_requires_registered_descriptor() {
        let mut router = AiExecutionRouter::default();
        router
            .set_acp_route("agent:codex", "codex.acp")
            .expect("route");
        let error = router
            .resolve("agent:codex", "agent:codex")
            .expect_err("missing descriptor");
        assert!(matches!(error, AiExecutionRoutingError::AcpAgentUnavailable { .. }));
    }

    #[test]
    fn managed_model_identity_can_share_one_registered_route() {
        let mut router = AiExecutionRouter::default();
        router
            .set_acp_route("model:managed_local", "goose.managed-local")
            .expect("route");
        let registry = DescriptorRegistry {
            descriptors: vec![descriptor("goose.managed-local")],
        };
        router.sync_registry(&registry);
        let resolution = router
            .resolve(
                "model:managed_local:model-a",
                "model:managed_local",
            )
            .expect("registered route");
        assert_eq!(
            resolution.driver,
            AiExecutionDriver::Acp {
                agent_id: "goose.managed-local".to_owned(),
            }
        );
    }
}
