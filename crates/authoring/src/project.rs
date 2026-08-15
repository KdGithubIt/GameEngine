//! Project root management for the authoring workflow.
//!
//! A [`ProjectRoot`] owns a project directory on disk, enforces a standard
//! asset layout, and provides safe path resolution that rejects traversal
//! outside the `assets/` subtree.
//!
//! The persistent state is stored in `project.json` at the project root:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "name": "MyGame"
//! }
//! ```
//!
//! See `docs/adr/0023-project-root-ownership.md` and §16–17 of
//! `AI_FRIENDLY_AUTHORING_SPEC.md` for the rationale.

use crate::persist::replace_file_contents;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Schema version written into every `project.json`.
///
/// `open` accepts only files whose `schema_version` equals this value.
pub const PROJECT_SCHEMA_VERSION: u32 = 1;

const PROJECT_JSON: &str = "project.json";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Metadata stored in `project.json` for a project directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    /// Human-readable project name.
    pub name: String,
    /// The `schema_version` read from (or written to) `project.json`.
    pub schema_version: u32,
}

/// Owns and validates a project directory on disk.
///
/// All asset path resolution goes through [`resolve_asset`] (for reading
/// existing files) or [`resolve_asset_for_write`] (for writing files that
/// may not exist yet). Both methods reject absolute paths, `..` traversal,
/// and resolved paths that escape the `assets/` subtree.
///
/// [`resolve_asset`]: ProjectRoot::resolve_asset
/// [`resolve_asset_for_write`]: ProjectRoot::resolve_asset_for_write
#[derive(Debug, Clone)]
pub struct ProjectRoot {
    path: PathBuf,
    config: ProjectConfig,
}

/// Describes a failure from [`ProjectRoot`] operations.
#[derive(Debug)]
pub enum ProjectError {
    /// The given path is not a directory.
    NotADirectory(PathBuf),
    /// The `project.json` file is absent in the project directory.
    MissingProjectFile(PathBuf),
    /// `project.json` could not be parsed (or serialized).
    JsonParse(serde_json::Error),
    /// The `schema_version` in `project.json` exceeds the supported version.
    UnsupportedVersion {
        /// The version found in the file.
        found: u32,
        /// The highest version this build supports.
        supported: u32,
    },
    /// The requested relative path escapes the `assets/` subtree.
    PathTraversal {
        /// The relative path string that was rejected.
        requested: String,
    },
    /// An underlying I/O operation failed.
    Io(io::Error),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADirectory(p) => write!(f, "`{}` is not a directory", p.display()),
            Self::MissingProjectFile(p) => {
                write!(f, "project file not found: `{}`", p.display())
            }
            Self::JsonParse(e) => write!(f, "project JSON error: {e}"),
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "project schema_version {found} is not supported by this build (max: {supported})"
            ),
            Self::PathTraversal { requested } => {
                write!(f, "path `{requested}` escapes the assets directory")
            }
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for ProjectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::JsonParse(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::NotADirectory(_)
            | Self::MissingProjectFile(_)
            | Self::UnsupportedVersion { .. }
            | Self::PathTraversal { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal serde types
// ---------------------------------------------------------------------------

/// On-disk representation of `project.json`.
///
/// `schema_version` is the first field so serde_json writes it before `name`,
/// following the ADR 0020 convention.
#[derive(Serialize, Deserialize)]
struct ProjectFile {
    schema_version: u32,
    name: String,
}

// ---------------------------------------------------------------------------
// ProjectRoot implementation
// ---------------------------------------------------------------------------

impl ProjectRoot {
    /// Returns the canonical path of the project root directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the project metadata loaded from (or written to) `project.json`.
    pub fn config(&self) -> &ProjectConfig {
        &self.config
    }

    /// Returns `<root>/assets/`.
    pub fn assets_root(&self) -> PathBuf {
        self.path.join("assets")
    }

    /// Returns `<root>/assets/scenes/`.
    pub fn scenes_dir(&self) -> PathBuf {
        self.assets_root().join("scenes")
    }

    /// Returns `<root>/assets/graphs/`.
    pub fn graphs_dir(&self) -> PathBuf {
        self.assets_root().join("graphs")
    }

    /// Returns `<root>/assets/meshes/`.
    pub fn meshes_dir(&self) -> PathBuf {
        self.assets_root().join("meshes")
    }

    /// Returns `<root>/assets/textures/`.
    pub fn textures_dir(&self) -> PathBuf {
        self.assets_root().join("textures")
    }

    /// Returns `<root>/assets/audio/`.
    pub fn audio_dir(&self) -> PathBuf {
        self.assets_root().join("audio")
    }

    /// Returns `<root>/assets/scripts/`, the user-visible script root.
    pub fn scripts_dir(&self) -> PathBuf {
        self.assets_root().join("scripts")
    }

    /// Returns `<root>/assets/scripts/rhai/`.
    pub fn rhai_scripts_dir(&self) -> PathBuf {
        self.scripts_dir().join("rhai")
    }

    /// Returns `<root>/assets/scripts/rust/`, the user-authored Rust root.
    pub fn rust_scripts_dir(&self) -> PathBuf {
        self.scripts_dir().join("rust")
    }

    /// Returns the recommended `<root>/assets/scripts/rust/components/` folder.
    ///
    /// This is the default destination for newly created components. Sources
    /// may be moved to any folder below [`Self::rust_scripts_dir`] afterwards;
    /// the folder does not classify what a source declares.
    pub fn rust_components_dir(&self) -> PathBuf {
        self.rust_scripts_dir().join("components")
    }

    /// Returns the recommended `<root>/assets/scripts/rust/resources/` folder.
    ///
    /// See [`Self::rust_components_dir`] for the placement rules.
    pub fn rust_resources_dir(&self) -> PathBuf {
        self.rust_scripts_dir().join("resources")
    }

    /// Returns the recommended `<root>/assets/scripts/rust/systems/` folder.
    ///
    /// See [`Self::rust_components_dir`] for the placement rules.
    pub fn rust_systems_dir(&self) -> PathBuf {
        self.rust_scripts_dir().join("systems")
    }

    /// Returns the recommended `<root>/assets/scripts/rust/shared/` folder.
    ///
    /// See [`Self::rust_components_dir`] for the placement rules.
    pub fn rust_shared_dir(&self) -> PathBuf {
        self.rust_scripts_dir().join("shared")
    }

    /// Returns `<root>/game/`, the optional project-local Rust crate.
    pub fn game_dir(&self) -> PathBuf {
        self.path.join("game")
    }

    /// Returns the generated crate-root module index of the Cargo build host.
    ///
    /// The file mirrors the folder tree below [`Self::rust_scripts_dir`] and is
    /// included by `game/src/lib.rs`, so a user folder maps directly onto a
    /// Rust module path. It is regenerated from disk and must not be edited.
    pub fn game_module_index_path(&self) -> PathBuf {
        self.game_dir().join("src").join("project_modules.rs")
    }

    /// Opens an existing project directory.
    ///
    /// Reads `project.json` and validates its `schema_version`, which the
    /// current format requires.
    ///
    /// # Errors
    ///
    /// - [`ProjectError::NotADirectory`] when `path` is not a directory.
    /// - [`ProjectError::MissingProjectFile`] when `project.json` is absent.
    /// - [`ProjectError::JsonParse`] when `project.json` contains invalid JSON
    ///   or omits `schema_version`.
    /// - [`ProjectError::UnsupportedVersion`] when `schema_version` differs
    ///   from [`PROJECT_SCHEMA_VERSION`].
    /// - [`ProjectError::Io`] for any other I/O failure.
    pub fn open(path: &Path) -> Result<Self, ProjectError> {
        if !path.is_dir() {
            return Err(ProjectError::NotADirectory(path.to_path_buf()));
        }
        let canonical = fs::canonicalize(path).map_err(ProjectError::Io)?;

        let project_json = canonical.join(PROJECT_JSON);
        if !project_json.exists() {
            return Err(ProjectError::MissingProjectFile(project_json));
        }
        let json = fs::read_to_string(&project_json).map_err(ProjectError::Io)?;
        let file: ProjectFile = serde_json::from_str(&json).map_err(ProjectError::JsonParse)?;

        if file.schema_version != PROJECT_SCHEMA_VERSION {
            return Err(ProjectError::UnsupportedVersion {
                found: file.schema_version,
                supported: PROJECT_SCHEMA_VERSION,
            });
        }

        Ok(Self {
            path: canonical,
            config: ProjectConfig {
                name: file.name,
                schema_version: file.schema_version,
            },
        })
    }

    /// Creates a new project in an existing directory.
    ///
    /// Writes `project.json` with `schema_version` = [`PROJECT_SCHEMA_VERSION`]
    /// and creates the standard asset subdirectories:
    /// `assets/{scenes,graphs,meshes,textures,audio}`.
    ///
    /// # Errors
    ///
    /// - [`ProjectError::NotADirectory`] when `path` is not a directory.
    /// - [`ProjectError::JsonParse`] when JSON serialization fails.
    /// - [`ProjectError::Io`] for any I/O failure.
    pub fn create(path: &Path, config: ProjectConfig) -> Result<Self, ProjectError> {
        if !path.is_dir() {
            return Err(ProjectError::NotADirectory(path.to_path_buf()));
        }
        let canonical = fs::canonicalize(path).map_err(ProjectError::Io)?;

        let standard_dirs = [
            canonical.join("assets").join("scenes"),
            canonical.join("assets").join("graphs"),
            canonical.join("assets").join("meshes"),
            canonical.join("assets").join("textures"),
            canonical.join("assets").join("audio"),
        ];
        for dir in &standard_dirs {
            fs::create_dir_all(dir).map_err(ProjectError::Io)?;
        }

        let file = ProjectFile {
            schema_version: PROJECT_SCHEMA_VERSION,
            name: config.name.clone(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(ProjectError::JsonParse)?;
        let project_json = canonical.join(PROJECT_JSON);
        replace_file_contents(&project_json, &json)
            .map_err(|e| ProjectError::Io(io::Error::other(e.to_string())))?;

        Ok(Self {
            path: canonical,
            config: ProjectConfig {
                name: config.name,
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        })
    }

    /// Resolves a relative path inside `assets/` to a canonical absolute path.
    ///
    /// Use this method for **reading** existing assets. The file must already
    /// exist because [`fs::canonicalize`] is used to verify the final path.
    ///
    /// # Security
    ///
    /// - Absolute paths are rejected.
    /// - `..` components are rejected.
    /// - After canonicalization, paths that escape `assets/` (including via
    ///   symlinks) are rejected.
    ///
    /// # Errors
    ///
    /// - [`ProjectError::PathTraversal`] for unsafe paths.
    /// - [`ProjectError::Io`] when `canonicalize` fails (file not found, etc.).
    pub fn resolve_asset(&self, relative: &str) -> Result<PathBuf, ProjectError> {
        reject_unsafe_path(relative)?;

        let candidate = self.assets_root().join(relative);
        let canonical = fs::canonicalize(&candidate).map_err(ProjectError::Io)?;
        let canonical_root = fs::canonicalize(self.assets_root()).map_err(ProjectError::Io)?;

        if !canonical.starts_with(&canonical_root) {
            return Err(ProjectError::PathTraversal {
                requested: relative.to_string(),
            });
        }
        Ok(canonical)
    }

    /// Resolves a relative path inside `assets/` for **writing** a file that
    /// may not yet exist.
    ///
    /// Because [`fs::canonicalize`] fails on paths that do not exist, this
    /// method canonicalizes the **parent directory** (which must exist) and
    /// appends the final path component after lexical validation.
    ///
    /// # Security
    ///
    /// - Absolute paths are rejected.
    /// - `..` components are rejected.
    /// - After canonicalizing the parent directory, paths that escape `assets/`
    ///   are rejected.
    ///
    /// # Errors
    ///
    /// - [`ProjectError::PathTraversal`] for unsafe paths or when the final
    ///   component is empty or invalid.
    /// - [`ProjectError::Io`] when the parent directory cannot be canonicalized.
    pub fn resolve_asset_for_write(&self, relative: &str) -> Result<PathBuf, ProjectError> {
        reject_unsafe_path(relative)?;

        let candidate = self.assets_root().join(relative);

        let parent = candidate
            .parent()
            .ok_or_else(|| ProjectError::PathTraversal {
                requested: relative.to_string(),
            })?;
        let canonical_parent = fs::canonicalize(parent).map_err(ProjectError::Io)?;
        let canonical_root = fs::canonicalize(self.assets_root()).map_err(ProjectError::Io)?;

        if !canonical_parent.starts_with(&canonical_root) {
            return Err(ProjectError::PathTraversal {
                requested: relative.to_string(),
            });
        }

        let file_name = candidate
            .file_name()
            .ok_or_else(|| ProjectError::PathTraversal {
                requested: relative.to_string(),
            })?;
        Ok(canonical_parent.join(file_name))
    }
}

// ---------------------------------------------------------------------------
// Path safety helpers
// ---------------------------------------------------------------------------

/// Rejects a relative path string that is rooted (absolute or drive-relative) or contains `..`.
fn reject_unsafe_path(relative: &str) -> Result<(), ProjectError> {
    let p = Path::new(relative);
    let has_windows_drive_prefix =
        matches!(relative.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic());
    if p.has_root() || has_windows_drive_prefix {
        return Err(ProjectError::PathTraversal {
            requested: relative.to_string(),
        });
    }
    for component in p.components() {
        // `has_root()` is false for drive-relative paths such as `C:foo`,
        // so prefix components must be rejected explicitly.
        if matches!(component, Component::ParentDir | Component::Prefix(_)) {
            return Err(ProjectError::PathTraversal {
                requested: relative.to_string(),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_config(name: &str) -> ProjectConfig {
        ProjectConfig {
            name: name.to_string(),
            schema_version: PROJECT_SCHEMA_VERSION,
        }
    }

    // --- create / open -------------------------------------------------------

    #[test]
    fn create_writes_project_json() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root =
            ProjectRoot::create(dir.path(), make_config("TestGame")).expect("create must succeed");

        let json_path = root.path().join("project.json");
        let json = fs::read_to_string(json_path).expect("project.json must exist");
        assert!(
            json.contains("\"TestGame\""),
            "name must appear in JSON: {json}"
        );
        assert!(
            json.contains("\"schema_version\""),
            "schema_version must appear in JSON: {json}"
        );
        assert!(
            json.contains("\"schema_version\": 1"),
            "schema_version must be 1: {json}"
        );
    }

    #[test]
    fn schema_version_appears_before_name_in_project_json() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root =
            ProjectRoot::create(dir.path(), make_config("OrderTest")).expect("create must succeed");

        let json = fs::read_to_string(root.path().join("project.json")).unwrap();
        let sv_pos = json
            .find("schema_version")
            .expect("schema_version must exist");
        let name_pos = json.find("name").expect("name must exist");
        assert!(sv_pos < name_pos, "schema_version must appear before name");
    }

    #[test]
    fn create_initializes_standard_directories() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        ProjectRoot::create(dir.path(), make_config("DirTest")).expect("create must succeed");

        for sub in &["scenes", "graphs", "meshes", "textures", "audio"] {
            let path = dir.path().join("assets").join(sub);
            assert!(
                path.is_dir(),
                "standard directory must exist: {}",
                path.display()
            );
        }
    }

    #[test]
    fn open_reads_existing_project() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        ProjectRoot::create(dir.path(), make_config("MyGame")).expect("create must succeed");

        let root = ProjectRoot::open(dir.path()).expect("open must succeed");
        assert_eq!(root.config().name, "MyGame");
        assert_eq!(root.config().schema_version, 1);
    }

    #[test]
    fn open_rejects_not_a_directory() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let file_path = dir.path().join("not_a_dir.txt");
        fs::write(&file_path, b"x").unwrap();

        let err = ProjectRoot::open(&file_path).expect_err("must reject a file");
        assert!(
            matches!(err, ProjectError::NotADirectory(_)),
            "expected NotADirectory, got: {err}"
        );
    }

    #[test]
    fn open_rejects_missing_project_json() {
        let dir = tempfile::tempdir().expect("temp dir must be created");

        let err = ProjectRoot::open(dir.path()).expect_err("must reject missing project.json");
        assert!(
            matches!(err, ProjectError::MissingProjectFile(_)),
            "expected MissingProjectFile, got: {err}"
        );
    }

    #[test]
    fn open_rejects_non_current_schema_version() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        fs::write(
            dir.path().join("project.json"),
            r#"{"schema_version":0,"name":"OldGame"}"#,
        )
        .unwrap();

        let err = ProjectRoot::open(dir.path()).expect_err("must reject old version");
        assert!(
            matches!(
                err,
                ProjectError::UnsupportedVersion {
                    found: 0,
                    supported: _
                }
            ),
            "expected UnsupportedVersion, got: {err}"
        );
    }

    #[test]
    fn open_without_schema_version_is_rejected() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        fs::write(dir.path().join("project.json"), r#"{"name":"OldGame"}"#).unwrap();

        let err = ProjectRoot::open(dir.path()).expect_err("missing schema_version must fail");
        assert!(matches!(err, ProjectError::JsonParse(_)));
    }

    #[test]
    fn open_rejects_malformed_project_json() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        fs::write(dir.path().join("project.json"), b"not valid json").unwrap();

        let err = ProjectRoot::open(dir.path()).expect_err("must reject malformed JSON");
        assert!(
            matches!(err, ProjectError::JsonParse(_)),
            "expected JsonParse, got: {err}"
        );
    }

    #[test]
    fn create_rejects_not_a_directory() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let file_path = dir.path().join("file.txt");
        fs::write(&file_path, b"x").unwrap();

        let err = ProjectRoot::create(&file_path, make_config("X")).expect_err("must reject file");
        assert!(
            matches!(err, ProjectError::NotADirectory(_)),
            "expected NotADirectory, got: {err}"
        );
    }

    #[test]
    fn path_returns_canonical_project_root() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("Canonical")).unwrap();
        assert!(root.path().is_absolute(), "path must be absolute");
    }

    // --- directory helpers ---------------------------------------------------

    #[test]
    fn dir_helpers_return_correct_subdirectories() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("DirHelpers")).unwrap();

        let base = root.path();
        assert_eq!(root.assets_root(), base.join("assets"));
        assert_eq!(root.scenes_dir(), base.join("assets").join("scenes"));
        assert_eq!(root.graphs_dir(), base.join("assets").join("graphs"));
        assert_eq!(root.meshes_dir(), base.join("assets").join("meshes"));
        assert_eq!(root.textures_dir(), base.join("assets").join("textures"));
        assert_eq!(root.audio_dir(), base.join("assets").join("audio"));
    }

    // --- resolve_asset (read) ------------------------------------------------

    #[test]
    fn resolve_asset_accepts_valid_existing_file() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("ResolveTest")).unwrap();

        let scene_file = root.scenes_dir().join("test.scene.json");
        fs::write(&scene_file, b"{}").unwrap();

        let resolved = root
            .resolve_asset("scenes/test.scene.json")
            .expect("valid path must resolve");
        assert!(
            resolved.starts_with(root.assets_root()),
            "resolved path must be within assets/"
        );
    }

    #[test]
    fn resolve_asset_rejects_absolute_path() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("AbsTest")).unwrap();

        let err = root
            .resolve_asset("/etc/passwd")
            .expect_err("absolute path must be rejected");
        assert!(
            matches!(err, ProjectError::PathTraversal { .. }),
            "expected PathTraversal, got: {err}"
        );
    }

    #[test]
    fn resolve_asset_rejects_single_parent_traversal() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("TraversalTest")).unwrap();

        let err = root
            .resolve_asset("../secret.txt")
            .expect_err("parent traversal must be rejected");
        assert!(
            matches!(err, ProjectError::PathTraversal { .. }),
            "expected PathTraversal, got: {err}"
        );
    }

    #[test]
    fn resolve_asset_rejects_nested_parent_traversal() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("NestedTraversalTest")).unwrap();

        let err = root
            .resolve_asset("scenes/../../secret.txt")
            .expect_err("nested traversal must be rejected");
        assert!(
            matches!(err, ProjectError::PathTraversal { .. }),
            "expected PathTraversal, got: {err}"
        );
    }

    #[test]
    fn resolve_asset_rejects_nonexistent_file() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("NonExistent")).unwrap();

        let err = root
            .resolve_asset("scenes/does_not_exist.scene.json")
            .expect_err("missing file must produce an error");
        assert!(
            matches!(err, ProjectError::Io(_)),
            "expected Io error for missing file, got: {err}"
        );
    }

    #[test]
    fn resolve_asset_rejects_windows_style_absolute_path() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("WinAbsTest")).unwrap();

        let err = root
            .resolve_asset("C:\\Windows\\System32\\secret.dll")
            .expect_err("Windows-style absolute path must be rejected");
        assert!(
            matches!(err, ProjectError::PathTraversal { .. }),
            "expected PathTraversal, got: {err}"
        );
    }

    #[test]
    fn resolve_asset_rejects_drive_relative_path() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("DriveRelTest")).unwrap();

        // Drive-relative paths such as `C:secret.dll` have a prefix component
        // but no root, so `has_root()` alone does not reject them.
        let err = root
            .resolve_asset("C:secret.dll")
            .expect_err("drive-relative path must be rejected");
        assert!(
            matches!(err, ProjectError::PathTraversal { .. }),
            "expected PathTraversal, got: {err}"
        );
    }

    // --- resolve_asset_for_write (write) ------------------------------------

    #[test]
    fn resolve_asset_for_write_accepts_new_file_in_existing_dir() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("WriteTest")).unwrap();

        let resolved = root
            .resolve_asset_for_write("scenes/new_scene.scene.json")
            .expect("new file path must resolve");
        assert!(
            resolved.starts_with(root.assets_root()),
            "resolved write path must be within assets/"
        );
        assert_eq!(
            resolved.file_name().unwrap().to_str().unwrap(),
            "new_scene.scene.json"
        );
    }

    #[test]
    fn resolve_asset_for_write_accepts_file_that_already_exists() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("WriteExistTest")).unwrap();

        let scene_file = root.scenes_dir().join("existing.scene.json");
        fs::write(&scene_file, b"{}").unwrap();

        let resolved = root
            .resolve_asset_for_write("scenes/existing.scene.json")
            .expect("existing file must also resolve for write");
        assert!(resolved.starts_with(root.assets_root()));
    }

    #[test]
    fn resolve_asset_for_write_rejects_absolute_path() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("WriteAbsTest")).unwrap();

        let err = root
            .resolve_asset_for_write("/tmp/escape.txt")
            .expect_err("absolute path must be rejected");
        assert!(
            matches!(err, ProjectError::PathTraversal { .. }),
            "expected PathTraversal, got: {err}"
        );
    }

    #[test]
    fn resolve_asset_for_write_rejects_parent_traversal() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("WriteTraversalTest")).unwrap();

        let err = root
            .resolve_asset_for_write("../outside.txt")
            .expect_err("parent traversal must be rejected");
        assert!(
            matches!(err, ProjectError::PathTraversal { .. }),
            "expected PathTraversal, got: {err}"
        );
    }

    #[test]
    fn resolve_asset_for_write_rejects_deeply_nested_traversal() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("WriteNestedTest")).unwrap();

        let err = root
            .resolve_asset_for_write("scenes/../../escape.txt")
            .expect_err("nested traversal must be rejected");
        assert!(
            matches!(err, ProjectError::PathTraversal { .. }),
            "expected PathTraversal, got: {err}"
        );
    }

    #[test]
    fn resolve_asset_for_write_rejects_parent_dir_that_does_not_exist() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let root = ProjectRoot::create(dir.path(), make_config("WriteNoParentTest")).unwrap();

        let err = root
            .resolve_asset_for_write("scenes/nonexistent_subdir/file.json")
            .expect_err("missing parent dir must produce Io error");
        assert!(
            matches!(err, ProjectError::Io(_)),
            "expected Io error for missing parent dir, got: {err}"
        );
    }

    // --- reject_unsafe_path (unit) ------------------------------------------

    #[test]
    fn reject_unsafe_path_accepts_simple_relative_path() {
        assert!(reject_unsafe_path("scenes/foo.json").is_ok());
        assert!(reject_unsafe_path("foo.json").is_ok());
    }

    #[test]
    fn reject_unsafe_path_rejects_absolute_paths() {
        assert!(reject_unsafe_path("/etc/passwd").is_err());
    }

    #[test]
    fn reject_unsafe_path_rejects_parent_components() {
        assert!(reject_unsafe_path("../foo").is_err());
        assert!(reject_unsafe_path("a/../../foo").is_err());
    }
}
