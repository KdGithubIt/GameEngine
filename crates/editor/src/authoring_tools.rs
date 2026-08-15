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
}

impl AuthoringTool {
    /// Stable display order used by editor menus.
    pub const ALL: [Self; 4] = [
        Self::AbilityDesigner,
        Self::RuntimeEventTimeline,
        Self::UiContractDesigner,
        Self::AdvancedGeometryDesigner,
    ];

    /// Human-readable tool name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::AbilityDesigner => "Ability Designer",
            Self::RuntimeEventTimeline => "Runtime Event Timeline",
            Self::UiContractDesigner => "UI Contract Designer",
            Self::AdvancedGeometryDesigner => "Advanced Geometry Designer",
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AuthoringTool;

    #[test]
    fn catalog_order_and_labels_are_stable() {
        assert_eq!(AuthoringTool::ALL.len(), 4);
        assert_eq!(AuthoringTool::ALL[0].label(), "Ability Designer");
        assert_eq!(
            AuthoringTool::ALL[3].label(),
            "Advanced Geometry Designer"
        );
    }
}
