//! Project Rust component discovery by persisted sidecar identity.

use engine_authoring::{
    component_metadata_path, load_component_metadata, ComponentMetadataError, Diagnostic,
    DiagnosticTarget,
};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// One unambiguous project component source and sidecar pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentSourceLocation {
    /// Stable component ID resolved from sidecar metadata.
    pub component_id: String,
    /// Rust source path below `assets/scripts/rust`.
    pub relative_path: PathBuf,
    /// Sidecar path below `assets/scripts/rust`, when metadata exists.
    pub metadata_relative_path: Option<PathBuf>,
    /// Rust type declared in the paired source.
    pub rust_type: String,
    /// One-based line containing the Rust type declaration.
    pub line: usize,
}

/// Editor-local source lookup plus actionable indexing diagnostics.
#[derive(Debug, Clone, Default)]
pub struct ComponentSourceIndex {
    locations: BTreeMap<String, ComponentSourceLocation>,
    ambiguous: BTreeMap<String, Vec<ComponentSourceLocation>>,
    diagnostics: Vec<Diagnostic>,
}

impl ComponentSourceIndex {
    /// Scans the complete Rust script root using sidecars as the only identity.
    ///
    /// Components are found wherever the author placed them below
    /// `assets/scripts/rust/`, because the source declaration and its sidecar,
    /// not the folder, identify a component.
    pub fn build(source_root: &Path) -> Self {
        let mut candidates = Vec::new();
        let mut diagnostics = Vec::new();
        scan_rust_files(source_root, source_root, &mut candidates, &mut diagnostics);
        scan_orphan_sidecars(source_root, source_root, &mut diagnostics);
        candidates.sort_by(|left, right| {
            left.component_id
                .cmp(&right.component_id)
                .then_with(|| left.relative_path.cmp(&right.relative_path))
                .then_with(|| left.line.cmp(&right.line))
        });

        let mut grouped: BTreeMap<String, Vec<ComponentSourceLocation>> = BTreeMap::new();
        for location in candidates {
            grouped
                .entry(location.component_id.clone())
                .or_default()
                .push(location);
        }
        let mut locations = BTreeMap::new();
        let mut ambiguous = BTreeMap::new();
        for (component_id, entries) in grouped {
            if entries.len() == 1 {
                locations.insert(component_id, entries.into_iter().next().expect("one entry"));
            } else {
                let paths = entries
                    .iter()
                    .map(|entry| format!("{}:{}", entry.relative_path.display(), entry.line))
                    .collect::<Vec<_>>()
                    .join(", ");
                diagnostics.push(
                    Diagnostic::error(
                        "editor.component_source.duplicate_id",
                        format!(
                            "component ID `{component_id}` is declared more than once: {paths}"
                        ),
                    )
                    .with_target(source_target(&entries[0].relative_path, entries[0].line)),
                );
                ambiguous.insert(component_id, entries);
            }
        }
        Self {
            locations,
            ambiguous,
            diagnostics,
        }
    }

    /// Resolves an ID only when it has exactly one valid source/metadata pair.
    pub fn resolve(&self, component_id: &str) -> Option<&ComponentSourceLocation> {
        self.locations.get(component_id)
    }

    /// Returns whether an ID is unusable because declarations collide.
    pub fn is_ambiguous(&self, component_id: &str) -> bool {
        self.ambiguous.contains_key(component_id)
    }

    /// Returns Problems diagnostics produced by the latest scan.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

fn scan_rust_files(
    root: &Path,
    directory: &Path,
    out: &mut Vec<ComponentSourceLocation>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            scan_rust_files(root, &path, out, diagnostics);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && path.file_name().is_none_or(|name| name != "mod.rs")
        {
            index_file(root, &path, out, diagnostics);
        }
    }
}

fn index_file(
    root: &Path,
    path: &Path,
    out: &mut Vec<ComponentSourceLocation>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(relative_path) = safe_relative(root, path) else {
        return;
    };
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                "editor.component_source.read_failed",
                format!("could not index {}: {error}", relative_path.display()),
            ));
            return;
        }
    };
    let declarations = engine_authoring::rust_declarations(&source)
        .into_iter()
        .filter(|declaration| declaration.kind == engine_authoring::RustDeclarationKind::Component)
        .collect::<Vec<_>>();
    if declarations.is_empty() {
        return;
    }
    if declarations.len() != 1 {
        diagnostics.push(
            Diagnostic::error(
                "editor.component_source.multiple_components",
                format!(
                    "{} contains multiple GameComponent declarations; one sidecar can identify only one component",
                    relative_path.display()
                ),
            )
            .with_target(source_target(&relative_path, declarations[0].line)),
        );
        return;
    }
    let declaration = &declarations[0];
    let metadata_path = component_metadata_path(path);
    let metadata_relative_path = safe_relative(root, &metadata_path);
    let component_id = if metadata_path.exists() {
        match load_component_metadata(&metadata_path) {
            Ok(metadata) => metadata.component_id,
            Err(error) => {
                diagnostics.push(metadata_error_diagnostic(
                    &relative_path,
                    declaration.line,
                    error,
                ));
                return;
            }
        }
    } else {
        diagnostics.push(
            Diagnostic::error(
                "editor.component_source.sidecar_missing",
                format!(
                    "{} has no `.rs.meta.json` sidecar; restore it instead of assigning a new ID",
                    relative_path.display()
                ),
            )
            .with_target(source_target(&relative_path, declaration.line)),
        );
        return;
    };
    out.push(ComponentSourceLocation {
        component_id,
        relative_path,
        metadata_relative_path,
        rust_type: declaration.rust_name.clone(),
        line: declaration.line,
    });
}

fn metadata_error_diagnostic(
    relative_path: &Path,
    line: usize,
    error: ComponentMetadataError,
) -> Diagnostic {
    let code = match &error {
        ComponentMetadataError::Json { .. } => "editor.component_source.sidecar_json",
        ComponentMetadataError::InvalidComponentId { .. } => "editor.component_source.invalid_id",
        ComponentMetadataError::UnsupportedVersion { .. } => {
            "editor.component_source.sidecar_version"
        }
        ComponentMetadataError::Io { .. } | ComponentMetadataError::Persist(_) => {
            "editor.component_source.sidecar_read_failed"
        }
    };
    Diagnostic::error(code, error.to_string()).with_target(source_target(relative_path, line))
}

fn scan_orphan_sidecars(root: &Path, directory: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            scan_orphan_sidecars(root, &path, diagnostics);
        } else if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".rs.meta.json"))
        {
            let source_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".meta.json"));
            let source_exists = source_name
                .map(|name| path.with_file_name(name).is_file())
                .unwrap_or(false);
            if !source_exists {
                let relative = safe_relative(root, &path).unwrap_or(path.clone());
                diagnostics.push(
                    Diagnostic::error(
                        "editor.component_source.orphan_sidecar",
                        format!("{} has no paired Rust source", relative.display()),
                    )
                    .with_target(source_target(&relative, 1)),
                );
            }
        }
    }
}

fn safe_relative(root: &Path, path: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix(root).ok()?.to_path_buf();
    (!relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }))
    .then_some(relative)
}

fn source_target(path: &Path, line: usize) -> DiagnosticTarget {
    DiagnosticTarget::SourceFile {
        path: path.to_string_lossy().replace('\\', "/"),
        line: Some(line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{write_component_metadata, ComponentMetadata};

    fn write_component(root: &Path, name: &str, source: &str, id: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::write(&path, source).unwrap();
        write_component_metadata(
            &component_metadata_path(&path),
            &ComponentMetadata::new(id).unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn indexes_sidecar_component_after_type_rename() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_component(
            directory.path(),
            "player.rs",
            "#[derive(GameComponent)]\n#[game_component(display_name = \"Player\")]\npub struct RenamedPlayer { enabled: bool }\n",
            "game.c_01kxtq56q3qxhqnqh86mmp758j",
        );
        let index = ComponentSourceIndex::build(directory.path());
        let location = index.resolve("game.c_01kxtq56q3qxhqnqh86mmp758j").unwrap();
        assert_eq!(location.relative_path, Path::new("player.rs"));
        assert_eq!(
            location.metadata_relative_path.as_deref(),
            Some(Path::new("player.rs.meta.json"))
        );
        assert_eq!(location.rust_type, "RenamedPlayer");
        assert_eq!(location.line, 3);
        assert!(path.is_file());
    }

    #[test]
    fn duplicate_sidecar_ids_are_diagnostic_and_never_resolve_arbitrarily() {
        let directory = tempfile::tempdir().unwrap();
        let id = "game.c_01kxtq56q3qxhqnqh86mmp758j";
        write_component(
            directory.path(),
            "a.rs",
            "#[derive(GameComponent)]\nstruct A;\n",
            id,
        );
        write_component(
            directory.path(),
            "b.rs",
            "#[derive(GameComponent)]\nstruct B;\n",
            id,
        );
        let index = ComponentSourceIndex::build(directory.path());
        assert!(index.resolve(id).is_none());
        assert!(index.is_ambiguous(id));
        assert!(index
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "editor.component_source.duplicate_id"));
    }

    #[test]
    fn missing_and_corrupt_sidecars_produce_stable_diagnostics() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.rs");
        std::fs::write(
            &missing,
            "#[derive(GameComponent)]\n#[game_component(display_name = \"Missing\")]\nstruct Missing;\n",
        )
        .unwrap();
        let corrupt = directory.path().join("corrupt.rs");
        std::fs::write(
            &corrupt,
            "#[derive(GameComponent)]\n#[game_component(display_name = \"Corrupt\")]\nstruct Corrupt;\n",
        )
        .unwrap();
        std::fs::write(component_metadata_path(&corrupt), "{").unwrap();
        let index = ComponentSourceIndex::build(directory.path());
        let codes = index
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"editor.component_source.sidecar_missing"));
        assert!(codes.contains(&"editor.component_source.sidecar_json"));
    }
}
