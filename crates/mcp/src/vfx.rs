//! MCP adapters for VFX semantic authoring.

use crate::McpToolDescriptor;
use engine_authoring::{
    VfxApply, VfxAuthoringService, VfxCommand, VfxCompilation, VfxEffect, VfxSchemaCatalog,
    VfxTemplate, VfxValidation,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

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
}

impl VfxMcpTools {
    /// Creates VFX MCP handlers over the shared GUI-free service.
    pub fn new() -> Self {
        Self {
            service: VfxAuthoringService::new(),
        }
    }

    /// Returns the VFX tool family advertised to MCP transports.
    pub fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        vec![
            descriptor("vfx.schemas", "Discover stable VFX module schemas and execution phases.", json!({"type":"object","properties":{},"additionalProperties":false})),
            descriptor("vfx.inspect", "Inspect one semantic VFX document and its compiled backend-neutral plan.", effect_schema()),
            descriptor("vfx.validate", "Validate one semantic VFX document with stable-ID diagnostics.", effect_schema()),
            descriptor("vfx.preview", "Preview a VFX command transaction without committing the source document.", mutation_schema()),
            descriptor("vfx.apply", "Apply one atomic VFX command transaction and return the committed document plus undo commands.", mutation_schema()),
            descriptor("vfx.template", "Create ordinary VFX document data from a built-in starting template.", json!({
                "type":"object",
                "required":["template"],
                "properties":{"template":{"type":"string","enum":["spark","smoke","burst","trail"]}},
                "additionalProperties":false
            })),
        ]
    }

    /// Returns the deterministic shared VFX module catalog.
    pub fn schemas(&self) -> VfxSchemaCatalog {
        self.service.schemas()
    }

    /// Inspects and compiles one VFX document without mutation.
    pub fn inspect(&self, input: VfxEffectInput) -> VfxInspectOutput {
        let validation = self.service.validate(&input.effect);
        let success = validation.success;
        VfxInspectOutput {
            success,
            compilation: self.service.compile(&input.effect),
            validation,
            effect: input.effect,
        }
    }

    /// Validates one VFX document.
    pub fn validate(&self, input: VfxEffectInput) -> VfxValidation {
        self.service.validate(&input.effect)
    }

    /// Previews an atomic semantic mutation without changing the source document.
    pub fn preview(&self, input: VfxMutationInput) -> VfxApply {
        self.service.apply(&input.effect, &input.commands)
    }

    /// Applies an atomic semantic mutation and returns the committed document.
    ///
    /// Persistence remains the responsibility of the owning project/editor host;
    /// MCP does not implement a second VFX file-mutation policy.
    pub fn apply(&self, input: VfxMutationInput) -> VfxApply {
        self.service.apply(&input.effect, &input.commands)
    }

    /// Creates an editable VFX document from a shared service template.
    pub fn template(&self, input: VfxTemplateInput) -> VfxEffect {
        self.service.template(input.template)
    }
}

impl Default for VfxMcpTools {
    fn default() -> Self {
        Self::new()
    }
}

fn descriptor(
    name: &'static str,
    description: &'static str,
    input_schema: serde_json::Value,
) -> McpToolDescriptor {
    McpToolDescriptor {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
    }
}

fn effect_schema() -> serde_json::Value {
    json!({
        "type":"object",
        "required":["effect"],
        "properties":{"effect":{"type":"object"}},
        "additionalProperties":false
    })
}

fn mutation_schema() -> serde_json::Value {
    json!({
        "type":"object",
        "required":["effect","commands"],
        "properties":{
            "effect":{"type":"object"},
            "commands":{"type":"array","items":{"type":"object"}}
        },
        "additionalProperties":false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
                "vfx.schemas",
                "vfx.inspect",
                "vfx.validate",
                "vfx.preview",
                "vfx.apply",
                "vfx.template",
            ]
        );
    }

    #[test]
    fn preview_and_apply_delegate_identical_semantic_transactions() {
        let tools = VfxMcpTools::new();
        let effect = VfxAuthoringService::new().template(VfxTemplate::Burst);
        let commands = vec![VfxCommand::SetEffectSeed { seed: 99 }];
        let preview = tools.preview(VfxMutationInput {
            effect: effect.clone(),
            commands: commands.clone(),
        });
        let applied = tools.apply(VfxMutationInput { effect, commands });
        assert_eq!(preview, applied);
        assert_eq!(preview.effect.expect("preview result").seed, 99);
    }
}
