//! Editor composition boundary for authoritative working copies (ADR 0139).
//!
//! Mutable document ownership stays in the existing workspace/specialized editor
//! sessions. This module only borrows those owners and captures immutable snapshots
//! for consumers that otherwise resolve project assets from disk.

use crate::animation_set_editor::AnimationSetEditorState;
use crate::material_editor::MaterialEditorPanel;
use crate::workspace::DocumentWorkspace;
use engine::authoring_overlay::{AuthoringDocumentOverlay, AuthoringDocumentSnapshot};
use engine_authoring::{Graph, ProjectRoot};
use std::path::Path;

pub(crate) fn capture_authoring_overlay(
    workspace: &DocumentWorkspace,
    animation_set: Option<&AnimationSetEditorState>,
    materials: &MaterialEditorPanel,
    project: Option<&ProjectRoot>,
) -> AuthoringDocumentOverlay {
    let mut overlay = AuthoringDocumentOverlay::new();

    for session in workspace.sessions() {
        let Some(path) = session.current_document_path() else { continue; };
        let Some(serialized) = session.graph_working_copy_json() else { continue; };
        let revision = session.document_revision();
        let snapshot = match serialized {
            Ok(contents) => AuthoringDocumentSnapshot::text(revision, revision, contents),
            Err(message) => AuthoringDocumentSnapshot::invalid(revision, revision, message),
        };
        overlay.insert(path.to_path_buf(), snapshot);
    }

    if let Some(state) = animation_set {
        let (revision, generation) = state.revision_generation();
        let snapshot = match state.document.to_canonical_json() {
            Ok(contents) => AuthoringDocumentSnapshot::text(revision, generation, contents),
            Err(error) => AuthoringDocumentSnapshot::invalid(revision, generation, error.to_string()),
        };
        overlay.insert(state.absolute_path.clone(), snapshot);
    }

    if let Some(project) = project {
        for (relative, material) in &materials.materials {
            let Some((revision, generation)) = materials.revision_generation(relative) else { continue; };
            let serialized = material
                .validate()
                .map_err(|error| error.to_string())
                .and_then(|()| material.to_json().map_err(|error| error.to_string()));
            let snapshot = match serialized {
                Ok(contents) => AuthoringDocumentSnapshot::text(revision, generation, contents),
                Err(message) => AuthoringDocumentSnapshot::invalid(revision, generation, message),
            };
            overlay.insert(project.assets_root().join(relative), snapshot);
        }
    }

    overlay
}

pub(crate) fn graph_working_copy<'a>(
    workspace: &'a DocumentWorkspace,
    path: &Path,
) -> Option<(&'a Graph, u64)> {
    let session = workspace.session_for_path(path)?;
    session.graph_working_copy_json()?;
    Some((session.graph(), session.document_revision()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{DocumentWorkspace, WorkspaceDocumentKind};
    use crate::EditorSession;

    #[test]
    fn open_graph_is_captured_without_touching_disk() {
        let directory = tempfile::tempdir().expect("temp");
        let path = directory.path().join("test.graph.json");
        let mut session = EditorSession::empty_animation_graph();
        session.save_as(path.clone()).expect("save fixture");
        let mut workspace = DocumentWorkspace::new(EditorSession::empty_behavior_tree());
        workspace.open_document(WorkspaceDocumentKind::Graph, path.clone()).expect("open");
        let overlay = capture_authoring_overlay(&workspace, None, &MaterialEditorPanel::new(), None);
        assert!(overlay.get(&path).is_some());
    }
}
