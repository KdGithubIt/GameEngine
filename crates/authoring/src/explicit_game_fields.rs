//! Public script-generation wrappers for explicit GameComponent authoring fields.

use crate::game_project::{self, GameProjectError, RustScriptKind, RustScriptSchedule};
use crate::persist::replace_file_contents;
use crate::project::ProjectRoot;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const GENERATED_COMPONENT_FIELD: &str =
    "    /// Example public setting shown in the Inspector.\n    pub enabled: bool,\n";
const EXPLICIT_GENERATED_COMPONENT_FIELD: &str =
    "    /// Example public setting shown in the Inspector.\n    #[game_field]\n    pub enabled: bool,\n";

/// Creates one Rust script in the recommended folder for its kind.
pub fn create_rust_script(
    project: &ProjectRoot,
    kind: RustScriptKind,
    rust_name: &str,
    schedule: RustScriptSchedule,
) -> Result<PathBuf, GameProjectError> {
    create_rust_script_in(
        project,
        kind,
        Path::new(kind.recommended_folder()),
        rust_name,
        schedule,
    )
}

/// Creates one Rust script in a folder relative to `assets/scripts/rust/`.
///
/// Generated components mark their example Inspector setting with bare
/// `#[game_field]`; every other generated field remains runtime-only by default.
pub fn create_rust_script_in(
    project: &ProjectRoot,
    kind: RustScriptKind,
    script_folder: &Path,
    rust_name: &str,
    schedule: RustScriptSchedule,
) -> Result<PathBuf, GameProjectError> {
    let path = game_project::create_rust_script_in(
        project,
        kind,
        script_folder,
        rust_name,
        schedule,
    )?;
    if kind != RustScriptKind::Component {
        return Ok(path);
    }

    let source = fs::read_to_string(&path).map_err(|source| GameProjectError::Io {
        path: path.clone(),
        source,
    })?;
    let source = mark_generated_component_field(&source).ok_or_else(|| GameProjectError::Io {
        path: path.clone(),
        source: io::Error::new(
            io::ErrorKind::InvalidData,
            "generated component template did not contain its example field",
        ),
    })?;
    replace_file_contents(&path, &source).map_err(GameProjectError::Persist)?;
    Ok(path)
}

fn mark_generated_component_field(source: &str) -> Option<String> {
    source.contains(GENERATED_COMPONENT_FIELD).then(|| {
        source.replacen(
            GENERATED_COMPONENT_FIELD,
            EXPLICIT_GENERATED_COMPONENT_FIELD,
            1,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{mark_generated_component_field, EXPLICIT_GENERATED_COMPONENT_FIELD};

    #[test]
    fn generated_component_marks_only_the_example_setting() {
        let source = "pub struct Example {\n    /// Example public setting shown in the Inspector.\n    pub enabled: bool,\n}\n";

        let updated = mark_generated_component_field(source).unwrap();

        assert!(updated.contains(EXPLICIT_GENERATED_COMPONENT_FIELD));
        assert_eq!(updated.matches("#[game_field]").count(), 1);
    }

    #[test]
    fn unexpected_templates_are_rejected() {
        assert!(mark_generated_component_field("pub struct Example {}\n").is_none());
    }
}
