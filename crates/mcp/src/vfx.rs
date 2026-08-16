//! MCP adapters for VFX semantic authoring.
//!
//! The shared VFX service operates on documents supplied in the request rather
//! than on a permissioned session, so this adapter authorizes each call against
//! the permission its capability declares in the canonical registry (ADR 0132
//! sections 5 and 6). The permission itself is never retyped here.

use crate::capability::{authorize_capability, domain_tool_descriptors};
use crate::{McpToolDescriptor, McpToolError};
use engine_authoring::{
    AuthoringCapabilityRegistry, AuthoringDomain, AuthoringPermissions, VfxApply,
    VfxAuthoringService, VfxCommand, VfxCompilation, VfxEffect, VfxSchemaCatalog, VfxTemplate,
    VfxValidation,
};
use serde::{Deserialize, Serialize};

/// Input carrying one VFX document for read-only tools.
#[derive(Debug, Clone, Deserialize)]
pub struct VfxEffectInput {
    /// Semantic VFX document to inspect or validate.
    pub effect: VfxEffect,
}

/// Transaction input shared by VFX preview and apply tools.
#[derive(Debug, Clone, Deserialize)]
pub struct VfxMutationInput {
    /// Source VFX document. The tool never mutates this value in place.
    pub effect: VfxEffect,
    /// Semantic commands applied as one transaction.
    pub commands: Vec<VfxCommand>,
}

/// Input selecting a built-in VFX starting template.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct VfxTemplateInput {
    /// Template to instantiate as ordinary editable VFX data.
    pub template: VfxTemplate,
}

/// Shared VFX inspection result for MCP clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VfxInspectOutput {
    /// Whether the document has no blocking diagnostics.
    pub success: bool,
    /// Original semantic VFX document.
    pub effect: VfxEffect,
    /// Shared authoring validation output.
    pub validation: VfxValidation,
    /// Backend-neutral compilation and capability diagnostics.
    pub compilation: VfxCompilation,
}

/// Transport-shaped VFX tools backed only by [`VfxAuthoringService`].
pub struct VfxMcpTools {
    service: VfxAuthoringService,
    registry: AuthoringCapabilityRegistry,
}

impl VfxMcpTools {
    /// Creates VFX MCP handlers over the shared GUI-free service.
    pub fn new() -> Self {
        Self {
            service: VfxAuthoringService::new(),
            registry: AuthoringCapabilityRegistry::builtin(),
        }
    }

    /// Returns the VFX tool family advertised to MCP transports.
    ///
    /// Names, descriptions, and argument schemas come from the canonical
    /// authoring capability registry (ADR 0132).
    pub fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        domain_tool_descriptors(&self.registry, &[AuthoringDomain::Vfx])
    }

    /// Returns the deterministic shared VFX module catalog.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the permission declared
    /// for `vfx.schemas`.
    pub fn schemas(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<VfxSchemaCatalog, McpToolError> {
        authorize_capability(&self.registry, "vfx.schemas", permissions)?;
        Ok(self.service.schemas())
    }

    /// Inspects and compiles one VFX document without mutation.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the permission declared
    /// for `vfx.inspect`.
    pub fn inspect(
        &self,
        permissions: &AuthoringPermissions,
        input: VfxEffectInput,
    ) -> Result<VfxInspectOutput, McpToolError> {
        authorize_capability(&self.registry, "vfx.inspect", permissions)?;
        let validation = self.service.validate(&input.effect);
        let success = validation.success;
        Ok(VfxInspectOutput {
            success,
            compilation: self.service.compile(&input.effect),
            validation,
            effect: input.effect,
        })
    }

    /// Validates one VFX document.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the permission declared
    /// for `vfx.validate`.
    pub fn validate(
        &self,
        permissions: &AuthoringPermissions,
        input: VfxEffectInput,
    ) -> Result<VfxValidation, McpToolError> {
        authorize_capability(&self.registry, "vfx.validate", permissions)?;
        Ok(self.service.validate(&input.effect))
    }

    /// Previews an atomic semantic mutation without changing the source document.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the permission declared
    /// for `vfx.preview`.
    pub fn preview(
        &self,
        permissions: &AuthoringPermissions,
        input: VfxMutationInput,
    ) -> Result<VfxApply, McpToolError> {
        authorize_capability(&self.registry, "vfx.preview", permissions)?;
        Ok(self.service.apply(&input.effect, &input.commands))
    }

    /// Applies an atomic semantic mutation and returns the committed document.
    ///
    /// Persistence remains the responsibility of the owning project/editor host;
    /// MCP does not implement a second VFX file-mutation policy.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the permission declared
    /// for `vfx.apply`.
    pub fn apply(
        &self,
        permissions: &AuthoringPermissions,
        input: VfxMutationInput,
    ) -> Result<VfxApply, McpToolError> {
        authorize_capability(&self.registry, "vfx.apply", permissions)?;
        Ok(self.service.apply(&input.effect, &input.commands))
    }

    /// Creates an editable VFX document from a shared service template.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the permission declared
    /// for `vfx.template`.
    pub fn template(
        &self,
        permissions: &AuthoringPermissions,
        input: VfxTemplateInput,
    ) -> Result<VfxEffect, McpToolError> {
        authorize_capability(&self.registry, "vfx.template", permissions)?;
        Ok(self.service.template(input.template))
    }
}

impl Default for VfxMcpTools {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::AuthoringPermission;

    fn writable() -> AuthoringPermissions {
        AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite)
    }

    #[test]
    fn descriptors_expose_complete_vfx_authoring_family() {
        let names = VfxMcpTools::new()
            .tool_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "vfx.apply",
                "vfx.inspect",
                "vfx.preview",
                "vfx.schemas",
                "vfx.template",
                "vfx.validate",
            ]
        );
    }

    #[test]
    fn preview_and_apply_delegate_identical_semantic_transactions() {
        let tools = VfxMcpTools::new();
        let permissions = writable();
        let effect = VfxAuthoringService::new().template(VfxTemplate::Burst);
        let commands = vec![VfxCommand::SetEffectSeed { seed: 99 }];
        let preview = tools
            .preview(
                &permissions,
                VfxMutationInput {
                    effect: effect.clone(),
                    commands: commands.clone(),
                },
            )
            .expect("preview permission");
        let applied = tools
            .apply(&permissions, VfxMutationInput { effect, commands })
            .expect("apply permission");
        assert_eq!(preview, applied);
        assert_eq!(preview.effect.expect("preview result").seed, 99);
    }

    #[test]
    fn read_only_sessions_cannot_commit_vfx_mutations() {
        let tools = VfxMcpTools::new();
        let read_only = AuthoringPermissions::read_only();
        let effect = VfxAuthoringService::new().template(VfxTemplate::Spark);

        tools
            .validate(
                &read_only,
                VfxEffectInput {
                    effect: effect.clone(),
                },
            )
            .expect("read-only sessions may validate");
        let error = tools
            .apply(
                &read_only,
                VfxMutationInput {
                    effect,
                    commands: vec![VfxCommand::SetEffectSeed { seed: 7 }],
                },
            )
            .expect_err("read-only sessions must not commit");

        assert_eq!(error.code(), "authoring.permission_denied");
    }

    #[test]
    fn preview_permission_does_not_authorize_apply() {
        let tools = VfxMcpTools::new();
        let preview_only = AuthoringPermissions::read_only().with(AuthoringPermission::Preview);
        let effect = VfxAuthoringService::new().template(VfxTemplate::Smoke);
        let commands = vec![VfxCommand::SetEffectSeed { seed: 11 }];

        tools
            .preview(
                &preview_only,
                VfxMutationInput {
                    effect: effect.clone(),
                    commands: commands.clone(),
                },
            )
            .expect("preview permission");
        let error = tools
            .apply(&preview_only, VfxMutationInput { effect, commands })
            .expect_err("preview permission must not commit");

        assert_eq!(error.code(), "authoring.permission_denied");
    }
}
