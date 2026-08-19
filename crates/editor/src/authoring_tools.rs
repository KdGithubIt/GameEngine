//! Authoring-window catalog exposed by the main Engine Editor.

/// Modeless authoring windows available from the main editor shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthoringTool {
    /// Startup, Active, Recovery, and Cooldown timeline authoring.
    AbilityDesigner,
    /// Live combat-hit and animation-event inspection.
    RuntimeEventTimeline,
    /// Typed UI bindings, events, and focus-navigation authoring.
    UiContractDesigner,
    /// Layered NavMesh, links, static meshes, and spatial-query authoring.
    AdvancedGeometryDesigner,
    /// Sprite Atlas, Sprite Animation, Tile Set, and Tile Map authoring.
    Native2d,
    /// Typed multi-emitter VFX asset authoring.
    VfxBuilder,
}

impl AuthoringTool {
    /// Stable display order used by editor menus.
    pub const ALL: [Self; 6] = [
        Self::AbilityDesigner,
        Self::RuntimeEventTimeline,
        Self::UiContractDesigner,
        Self::AdvancedGeometryDesigner,
        Self::Native2d,
        Self::VfxBuilder,
    ];

    /// Human-readable tool name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::AbilityDesigner => "Ability Designer",
            Self::RuntimeEventTimeline => "Runtime Event Timeline",
            Self::UiContractDesigner => "UI Contract Designer",
            Self::AdvancedGeometryDesigner => "Advanced Geometry Designer",
            Self::Native2d => "Native 2D",
            Self::VfxBuilder => "VFX Builder",
        }
    }

    /// Short description presented in the main editor.
    pub const fn description(self) -> &'static str {
        match self {
            Self::AbilityDesigner => {
                "Author reusable Startup, Active, Recovery, and Cooldown timings."
            }
            Self::RuntimeEventTimeline => {
                "Inspect live animation events and accepted combat hits in fixed-step order."
            }
            Self::UiContractDesigner => {
                "Author typed bindings, UI events, initial focus, and directional navigation."
            }
            Self::AdvancedGeometryDesigner => {
                "Author layered NavMeshes, floor links, static triangles, paths, and raycasts."
            }
            Self::Native2d => {
                "Author Sprite Atlases, Sprite Animations, Tile Sets, and sparse Tile Maps."
            }
            Self::VfxBuilder => {
                "Author typed emitters and ordered spawn, update, and render modules."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AuthoringTool;

    #[test]
    fn catalog_order_and_labels_are_stable() {
        assert_eq!(AuthoringTool::ALL.len(), 6);
        assert_eq!(AuthoringTool::ALL[0].label(), "Ability Designer");
        assert_eq!(AuthoringTool::ALL[3].label(), "Advanced Geometry Designer");
        assert_eq!(AuthoringTool::ALL[4].label(), "Native 2D");
        assert_eq!(AuthoringTool::ALL[5].label(), "VFX Builder");
    }
}
