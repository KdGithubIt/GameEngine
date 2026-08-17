//! Process-local authoring document snapshots supplied by editor hosts.
//!
//! ADR 0139 keeps mutable working-copy ownership in the editor. Runtime and
//! validation code receive an immutable, engine-neutral overlay at composition
//! boundaries so an open working copy wins over the saved file without making
//! engine crates depend on editor session types.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One immutable authoring document value captured from an editor working copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringDocumentSnapshot {
    revision: u64,
    generation: u64,
    contents: Result<String, String>,
}

impl AuthoringDocumentSnapshot {
    /// Creates a valid text snapshot.
    pub fn text(revision: u64, generation: u64, contents: String) -> Self {
        Self { revision, generation, contents: Ok(contents) }
    }

    /// Creates an invalid snapshot that deliberately blocks disk fallback.
    pub fn invalid(revision: u64, generation: u64, message: String) -> Self {
        Self { revision, generation, contents: Err(message) }
    }

    /// Returns the process-local logical revision captured for this snapshot.
    pub fn revision(&self) -> u64 { self.revision }

    /// Returns the process-local generation captured for this snapshot.
    pub fn generation(&self) -> u64 { self.generation }

    /// Returns the captured text or the working-copy validation/serialization error.
    pub fn contents(&self) -> Result<&str, &str> {
        self.contents.as_deref().map_err(String::as_str)
    }
}

/// Immutable project document overlay used by one validation/preview/Play composition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthoringDocumentOverlay {
    documents: BTreeMap<PathBuf, AuthoringDocumentSnapshot>,
}

impl AuthoringDocumentOverlay {
    /// Creates an empty overlay.
    pub fn new() -> Self { Self::default() }

    /// Inserts or replaces the authoritative snapshot for `path`.
    pub fn insert(&mut self, path: PathBuf, snapshot: AuthoringDocumentSnapshot) {
        self.documents.insert(path, snapshot);
    }

    /// Returns the authoritative working-copy snapshot for `path`, if one exists.
    pub fn get(&self, path: &Path) -> Option<&AuthoringDocumentSnapshot> {
        self.documents.get(path)
    }

    /// Returns whether no working-copy snapshots were captured.
    pub fn is_empty(&self) -> bool { self.documents.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_snapshot_never_turns_into_missing_disk_fallback() {
        let path = PathBuf::from("assets/test.graph.json");
        let mut overlay = AuthoringDocumentOverlay::new();
        overlay.insert(path.clone(), AuthoringDocumentSnapshot::invalid(7, 3, "invalid graph".into()));
        let snapshot = overlay.get(&path).expect("working copy remains authoritative");
        assert_eq!(snapshot.revision(), 7);
        assert_eq!(snapshot.generation(), 3);
        assert_eq!(snapshot.contents(), Err("invalid graph"));
    }
}
