//! Read-only source documents for engine-owned component implementations.

use std::path::{Path, PathBuf};

/// Engine source displayed in an internal, explicitly read-only tab.
#[derive(Debug, Clone)]
pub struct ReadOnlySourceDocument {
    /// Stable built-in component ID being inspected.
    pub component_id: String,
    /// Engine/SDK version expected by the running editor.
    pub sdk_version: String,
    /// SDK-relative path, safe to display and persist in editor-local state.
    pub relative_path: PathBuf,
    /// Source text when the matching bundle is available.
    pub source: Option<String>,
    /// One-based best-effort declaration line.
    pub line: usize,
    /// Recovery explanation when the source bundle is missing.
    pub missing_reason: Option<String>,
}

/// SDK-relative sources searched for a builtin component id, in preference
/// order: the id constants, the authoring schemas, then the registry itself.
const COMPONENT_SOURCE_CANDIDATES: &[&str] = &[
    "crates/engine/src/scene_bridge.rs",
    "crates/engine/src/components/schemas.rs",
    "crates/engine/src/components.rs",
];

impl ReadOnlySourceDocument {
    /// Loads the built-in component source from the SDK bundle.
    ///
    /// The first candidate file that mentions `component_id` wins; when none
    /// does, the first readable candidate is still shown so the user can
    /// browse the registry.
    pub fn load(component_id: &str) -> Self {
        let sdk_version = env!("CARGO_PKG_VERSION").to_owned();
        let root = source_bundle_root();
        let mut resolved: Option<(PathBuf, String)> = None;
        if let Some(root) = root.as_ref() {
            for candidate in COMPONENT_SOURCE_CANDIDATES {
                let relative_path = PathBuf::from(candidate);
                let Ok(text) = std::fs::read_to_string(root.join(&relative_path)) else {
                    continue;
                };
                let mentions_component = text.contains(component_id);
                if resolved.is_none() || mentions_component {
                    resolved = Some((relative_path, text));
                }
                if mentions_component {
                    break;
                }
            }
        }
        let (relative_path, source) = match resolved {
            Some((relative_path, text)) => (relative_path, Some(text)),
            None => (PathBuf::from(COMPONENT_SOURCE_CANDIDATES[0]), None),
        };
        let line = source
            .as_deref()
            .and_then(|text| {
                text.lines()
                    .position(|line| line.contains(component_id))
                    .map(|index| index + 1)
            })
            .unwrap_or(1);
        let missing_reason = source.is_none().then(|| {
            format!(
                "The read-only engine source bundle for SDK {sdk_version} is missing. Repair or reinstall this editor SDK."
            )
        });
        Self {
            component_id: component_id.to_owned(),
            sdk_version,
            relative_path,
            source,
            line,
            missing_reason,
        }
    }
}

fn source_bundle_root() -> Option<PathBuf> {
    std::env::var_os("GAMEENGINE_SDK_SOURCE_ROOT")
        .map(PathBuf::from)
        .filter(|root| root.is_dir())
        .or_else(|| {
            let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            workspace.is_dir().then_some(workspace)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_sdk_source_resolves_without_exposing_it_as_editable() {
        let document = ReadOnlySourceDocument::load("engine.transform");
        assert_eq!(
            document.relative_path,
            Path::new("crates/engine/src/scene_bridge.rs")
        );
        assert!(document
            .source
            .as_deref()
            .is_some_and(|source| source.contains("engine.transform")));
    }
}
