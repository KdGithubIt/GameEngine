//! Loads authoring scenes from the project asset directory.
//!
//! Use [`SceneLoader`] to load `*.scene.json` files by a path relative to the
//! project's asset root.  Paths are validated through
//! [`engine_authoring::ProjectRoot::resolve_asset`] to prevent traversal
//! outside the project.

use engine_authoring::{load_scene_from_json, AuthoringScene, Diagnostic, ProjectRoot};
use std::{fmt, io};

/// Error returned by [`SceneLoader::load`].
#[derive(Debug)]
pub enum SceneLoadError {
    /// Path resolution or file I/O failed.
    Io(io::Error),
    /// The file is not valid scene JSON.
    JsonParse(String),
    /// The scene parsed but produced blocking validation errors.
    Validation(Vec<Diagnostic>),
}

impl fmt::Display for SceneLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "scene I/O error: {e}"),
            Self::JsonParse(msg) => write!(f, "scene parse error: {msg}"),
            Self::Validation(diags) => write!(f, "{} blocking validation error(s)", diags.len()),
        }
    }
}

impl std::error::Error for SceneLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::JsonParse(_) | Self::Validation(_) => None,
        }
    }
}

/// Loads authoring scenes from the project's asset directory.
///
/// All paths are validated through
/// [`ProjectRoot::resolve_asset`] to prevent traversal outside the project's
/// asset root.
pub struct SceneLoader {
    root: ProjectRoot,
}

impl SceneLoader {
    /// Creates a loader bound to `root`.
    pub fn new(root: ProjectRoot) -> Self {
        Self { root }
    }

    /// Loads and validates the scene at `relative_path` within the asset root.
    ///
    /// `relative_path` is resolved against the project's asset root using
    /// [`ProjectRoot::resolve_asset`].  The path must not contain `..`
    /// segments or be absolute.
    ///
    /// # Errors
    ///
    /// - [`SceneLoadError::Io`] when path resolution fails or the file cannot
    ///   be read.
    /// - [`SceneLoadError::JsonParse`] when the file does not contain valid
    ///   scene JSON.
    /// - [`SceneLoadError::Validation`] when the scene has blocking validation
    ///   errors.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(&self, relative_path: &str) -> Result<AuthoringScene, SceneLoadError> {
        let path = self.root.resolve_asset(relative_path).map_err(|source| {
            SceneLoadError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                source.to_string(),
            ))
        })?;
        let json = std::fs::read_to_string(&path).map_err(SceneLoadError::Io)?;
        let scene = load_scene_from_json(&json)
            .map_err(|source| SceneLoadError::JsonParse(source.to_string()))?;
        let diagnostics = scene.validate();
        if diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(SceneLoadError::Validation(diagnostics));
        }
        Ok(scene)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn load(&self, _relative_path: &str) -> Result<AuthoringScene, SceneLoadError> {
        Err(SceneLoadError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "scene loading is not available on wasm32",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{ProjectConfig, PROJECT_SCHEMA_VERSION};

    fn make_project() -> (tempfile::TempDir, ProjectRoot) {
        let dir = tempfile::tempdir().unwrap();
        let root = ProjectRoot::create(
            dir.path(),
            ProjectConfig {
                name: "TestProject".into(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project create must succeed");
        (dir, root)
    }

    #[test]
    fn scene_loader_loads_valid_scene() {
        let (dir, root) = make_project();
        let scene_path = root.scenes_dir().join("main.scene.json");
        std::fs::write(&scene_path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let loader = SceneLoader::new(root);
        let scene = loader
            .load("scenes/main.scene.json")
            .expect("load must succeed");
        assert_eq!(scene.entity_count(), 0);
        drop(dir);
    }

    #[test]
    fn scene_loader_rejects_path_traversal() {
        let (dir, root) = make_project();
        let loader = SceneLoader::new(root);
        let result = loader.load("../secret.txt");
        assert!(
            matches!(result, Err(SceneLoadError::Io(_))),
            "path traversal must fail with Io error"
        );
        drop(dir);
    }

    #[test]
    fn scene_loader_fails_on_missing_file() {
        let (dir, root) = make_project();
        let loader = SceneLoader::new(root);
        let result = loader.load("scenes/nonexistent.scene.json");
        assert!(
            matches!(result, Err(SceneLoadError::Io(_))),
            "missing file must fail with Io error"
        );
        drop(dir);
    }

    #[test]
    fn scene_loader_fails_on_malformed_json() {
        let (dir, root) = make_project();
        let scene_path = root.scenes_dir().join("bad.scene.json");
        std::fs::write(&scene_path, "not valid json").unwrap();
        let loader = SceneLoader::new(root);
        let result = loader.load("scenes/bad.scene.json");
        assert!(
            matches!(result, Err(SceneLoadError::JsonParse(_))),
            "malformed JSON must fail with JsonParse error"
        );
        drop(dir);
    }
}
