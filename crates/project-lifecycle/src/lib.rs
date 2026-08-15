//! GUI-free project acquisition and desktop application lifecycle.
//!
//! This crate sits above `engine-authoring`: it validates the engine
//! association in `project.json`, creates complete project scaffolds, owns the
//! authoritative per-location Editor lease, and carries only ephemeral
//! Launcher/Editor process-control state outside the project working tree.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

use engine_authoring::{
    initialize_game_project, replace_file_contents, AuthoringScene, ProjectConfig, ProjectError,
    ProjectId, ProjectRoot, ProjectSettings, PROJECT_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Engine release association written into new `project.json` files.
pub const CURRENT_ENGINE_ASSOCIATION: &str = env!("CARGO_PKG_VERSION");

const EDITOR_LOCK_FILE: &str = "editor.lock";
const EDITOR_OWNER_FILE: &str = "editor-owner.json";
const EDITOR_READY_FILE: &str = "editor-ready.json";
const EDITOR_ACTIVATE_FILE: &str = "activate.request";
const EDITOR_CLOSE_FILE: &str = "close.request";
const LAUNCHER_LOCK_FILE: &str = "launcher.lock";
const LAUNCHER_REQUEST_FILE: &str = "launcher.request.json";

/// Describes a project/application lifecycle failure.
#[derive(Debug)]
pub enum LifecycleError {
    /// The underlying canonical project is invalid.
    Project(ProjectError),
    /// The project was written for another engine association.
    IncompatibleEngine {
        /// Association persisted in `project.json`.
        found: String,
        /// Association required by this build.
        expected: &'static str,
    },
    /// A complete project cannot be published over an existing path.
    ProjectAlreadyExists(PathBuf),
    /// A project name cannot be used as one directory component.
    InvalidProjectName(String),
    /// The canonical project location already has an Editor process.
    EditorAlreadyOpen(PathBuf),
    /// A sibling desktop executable could not be located.
    ExecutableNotFound(PathBuf),
    /// One filesystem operation failed.
    Io {
        /// Path involved in the operation.
        path: PathBuf,
        /// I/O failure returned by the operating system.
        source: io::Error,
    },
    /// JSON lifecycle metadata could not be encoded or decoded.
    Json(serde_json::Error),
    /// A GUI-free scaffold step failed after staging started.
    Scaffold {
        /// Stable name of the failed scaffold step.
        step: &'static str,
        /// Underlying error text.
        message: String,
    },
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Project(error) => error.fmt(f),
            Self::IncompatibleEngine { found, expected } => write!(
                f,
                "project engine association `{found}` is incompatible with this build (`{expected}` required)"
            ),
            Self::ProjectAlreadyExists(path) => {
                write!(f, "project destination already exists: {}", path.display())
            }
            Self::InvalidProjectName(name) => write!(f, "invalid project name `{name}`"),
            Self::EditorAlreadyOpen(path) => {
                write!(f, "an Editor already owns {}", path.display())
            }
            Self::ExecutableNotFound(path) => {
                write!(f, "desktop executable not found: {}", path.display())
            }
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Json(error) => write!(f, "lifecycle metadata JSON error: {error}"),
            Self::Scaffold { step, message } => {
                write!(f, "project scaffold step `{step}` failed: {message}")
            }
        }
    }
}

impl std::error::Error for LifecycleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Project(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::Json(error) => Some(error),
            Self::IncompatibleEngine { .. }
            | Self::ProjectAlreadyExists(_)
            | Self::InvalidProjectName(_)
            | Self::EditorAlreadyOpen(_)
            | Self::ExecutableNotFound(_)
            | Self::Scaffold { .. } => None,
        }
    }
}

impl From<ProjectError> for LifecycleError {
    fn from(value: ProjectError) -> Self {
        Self::Project(value)
    }
}

impl From<serde_json::Error> for LifecycleError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Ephemeral diagnostic ownership data for one active Editor process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditorOwnerMetadata {
    /// Operating-system process identifier.
    pub process_id: u32,
    /// Stable project identity read from the current `project.json`.
    pub project_id: ProjectId,
    /// Canonical location protected by the authoritative lease.
    pub canonical_project: PathBuf,
    /// Unix timestamp in milliseconds when the lease was acquired.
    pub acquired_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EditorReadyMetadata {
    process_id: u32,
    ready_unix_ms: u64,
}

/// Outcome of asking lifecycle policy to open a project in the Editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorLaunchOutcome {
    /// A new Editor process was spawned.
    Spawned,
    /// The already-running Editor was asked to activate.
    Activated,
}

/// Result of an Editor launch/activation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorLaunch {
    /// Whether lifecycle spawned or activated an Editor.
    pub outcome: EditorLaunchOutcome,
    /// Canonical location associated with the request.
    pub canonical_project: PathBuf,
}

/// Request delivered to the single Launcher instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LauncherRequest {
    /// Project whose Editor should remain alive until a target is ready.
    pub switch_from: Option<PathBuf>,
    nonce: String,
}

/// Authoritative Editor lease held for the whole process lifetime.
///
/// The operating system releases the underlying file lock when the process
/// exits or crashes. JSON files beside the lock are diagnostic/control state
/// only and never determine ownership.
pub struct EditorLease {
    project: ProjectRoot,
    _lock: File,
    state_dir: PathBuf,
    last_activate: Option<String>,
    last_close: Option<String>,
}

impl EditorLease {
    /// Returns the concrete project acquired for this Editor process.
    pub fn project_root(&self) -> &ProjectRoot {
        &self.project
    }

    /// Publishes that project opening and initial Editor bootstrap succeeded.
    ///
    /// # Errors
    ///
    /// Returns an error when readiness metadata cannot be written.
    pub fn mark_ready(&self) -> Result<(), LifecycleError> {
        write_json(
            &self.state_dir.join(EDITOR_READY_FILE),
            &EditorReadyMetadata {
                process_id: std::process::id(),
                ready_unix_ms: unix_millis(),
            },
        )
    }

    /// Returns `true` once for each new activation request.
    pub fn take_activation_request(&mut self) -> bool {
        take_control_request(
            &self.state_dir.join(EDITOR_ACTIVATE_FILE),
            &mut self.last_activate,
        )
    }

    /// Returns `true` once for each new lifecycle close request.
    pub fn take_close_request(&mut self) -> bool {
        take_control_request(
            &self.state_dir.join(EDITOR_CLOSE_FILE),
            &mut self.last_close,
        )
    }
}

impl Drop for EditorLease {
    fn drop(&mut self) {
        remove_owned_metadata(
            &self.state_dir.join(EDITOR_OWNER_FILE),
            std::process::id(),
        );
        remove_owned_ready(
            &self.state_dir.join(EDITOR_READY_FILE),
            std::process::id(),
        );
    }
}

/// Exclusive single-instance lease and request channel for the Launcher.
pub struct LauncherSession {
    _lock: File,
    request_path: PathBuf,
    last_request: Option<String>,
    initial_request: Option<LauncherRequest>,
}

impl LauncherSession {
    /// Returns the next activation/switch request sent by another process.
    pub fn take_request(&mut self) -> Option<LauncherRequest> {
        if self.initial_request.is_some() {
            return self.initial_request.take();
        }
        let text = fs::read_to_string(&self.request_path).ok()?;
        if self.last_request.as_deref() == Some(text.as_str()) {
            return None;
        }
        self.last_request = Some(text.clone());
        serde_json::from_str(&text).ok()
    }
}

impl Drop for LauncherSession {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.request_path);
    }
}

/// Opens and validates the current project identity and engine association.
///
/// # Errors
///
/// Returns the underlying [`ProjectError`] or [`LifecycleError::IncompatibleEngine`].
pub fn inspect_project(path: &Path) -> Result<ProjectRoot, LifecycleError> {
    let project = ProjectRoot::open(path)?;
    if project.config().engine_version != CURRENT_ENGINE_ASSOCIATION {
        return Err(LifecycleError::IncompatibleEngine {
            found: project.config().engine_version.clone(),
            expected: CURRENT_ENGINE_ASSOCIATION,
        });
    }
    Ok(project)
}

/// Creates a complete current-format project through a staging directory.
///
/// The final path is published with one rename only after project identity,
/// standard directories, the Rust host, starter Scene, settings, and a final
/// compatibility check all succeed.
///
/// # Errors
///
/// Returns an error without publishing `final_path` if any scaffold step fails.
pub fn create_standard_project(
    final_path: &Path,
    name: &str,
) -> Result<ProjectRoot, LifecycleError> {
    validate_project_name(name)?;
    if final_path.exists() {
        return Err(LifecycleError::ProjectAlreadyExists(final_path.to_path_buf()));
    }
    let parent = final_path.parent().ok_or_else(|| LifecycleError::InvalidProjectName(name.into()))?;
    if !parent.is_dir() {
        return Err(LifecycleError::Io {
            path: parent.to_path_buf(),
            source: io::Error::new(io::ErrorKind::NotFound, "project parent directory does not exist"),
        });
    }

    let staging = parent.join(format!(
        ".gameengine-project-{}-{}-{}.staging",
        std::process::id(),
        unix_millis(),
        name
    ));
    fs::create_dir(&staging).map_err(|source| LifecycleError::Io {
        path: staging.clone(),
        source,
    })?;

    let staged_result = (|| {
        let project = ProjectRoot::create(
            &staging,
            ProjectConfig {
                project_id: ProjectId::generate(),
                name: name.to_owned(),
                engine_version: CURRENT_ENGINE_ASSOCIATION.to_owned(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )?;
        initialize_game_project(&project).map_err(|error| LifecycleError::Scaffold {
            step: "game_project",
            message: error.to_string(),
        })?;

        let scene = AuthoringScene::new();
        let scene_json = scene
            .to_canonical_json()
            .map_err(|error| LifecycleError::Scaffold {
                step: "starter_scene",
                message: error.to_string(),
            })?;
        let scene_path = project
            .resolve_asset_for_write("scenes/main.scene.json")
            .map_err(LifecycleError::Project)?;
        replace_file_contents(&scene_path, &scene_json).map_err(|error| {
            LifecycleError::Scaffold {
                step: "starter_scene",
                message: error.to_string(),
            }
        })?;

        let settings = ProjectSettings {
            start_scene: Some("scenes/main.scene.json".to_owned()),
            ..ProjectSettings::default()
        };
        settings
            .save(project.path())
            .map_err(|error| LifecycleError::Scaffold {
                step: "project_settings",
                message: error.to_string(),
            })?;
        inspect_project(&staging)?;
        Ok::<(), LifecycleError>(())
    })();

    if let Err(error) = staged_result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    fs::rename(&staging, final_path).map_err(|source| {
        let _ = fs::remove_dir_all(&staging);
        LifecycleError::Io {
            path: final_path.to_path_buf(),
            source,
        }
    })?;
    inspect_project(final_path)
}

/// Acquires the authoritative exclusive lease for a direct or Launcher-spawned
/// Editor process.
///
/// If another Editor already owns the canonical location, it receives an
/// activation request and this call fails rather than opening a duplicate.
///
/// # Errors
///
/// Returns project/compatibility failures, lock I/O errors, or
/// [`LifecycleError::EditorAlreadyOpen`].
pub fn acquire_editor_project(path: &Path) -> Result<EditorLease, LifecycleError> {
    let project = inspect_project(path)?;
    let state_dir = project_state_dir(&project);
    fs::create_dir_all(&state_dir).map_err(|source| LifecycleError::Io {
        path: state_dir.clone(),
        source,
    })?;
    let lock_path = state_dir.join(EDITOR_LOCK_FILE);
    let lock = open_lock_file(&lock_path)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let _ = write_control_request(&state_dir.join(EDITOR_ACTIVATE_FILE));
            return Err(LifecycleError::EditorAlreadyOpen(
                project.path().to_path_buf(),
            ));
        }
        Err(TryLockError::Error(source)) => {
            return Err(LifecycleError::Io {
                path: lock_path,
                source,
            });
        }
    }

    let _ = fs::remove_file(state_dir.join(EDITOR_READY_FILE));
    write_json(
        &state_dir.join(EDITOR_OWNER_FILE),
        &EditorOwnerMetadata {
            process_id: std::process::id(),
            project_id: project.config().project_id.clone(),
            canonical_project: project.path().to_path_buf(),
            acquired_unix_ms: unix_millis(),
        },
    )?;
    Ok(EditorLease {
        project,
        _lock: lock,
        state_dir,
        last_activate: None,
        last_close: None,
    })
}

/// Returns diagnostic metadata for the active Editor owner, when the location
/// is currently leased.
///
/// # Errors
///
/// Returns an error if project inspection or lock probing fails.
pub fn editor_owner_metadata(
    path: &Path,
) -> Result<Option<EditorOwnerMetadata>, LifecycleError> {
    let project = inspect_project(path)?;
    let state_dir = project_state_dir(&project);
    if !editor_lock_is_held(&state_dir)? {
        return Ok(None);
    }
    read_json_optional(&state_dir.join(EDITOR_OWNER_FILE))
}

/// Returns whether the active Editor reported successful bootstrap.
///
/// Stale readiness JSON is ignored unless the OS lease is still held and its
/// process ID matches current owner metadata.
///
/// # Errors
///
/// Returns an error if project inspection or lock probing fails.
pub fn editor_is_ready(path: &Path) -> Result<bool, LifecycleError> {
    let project = inspect_project(path)?;
    let state_dir = project_state_dir(&project);
    if !editor_lock_is_held(&state_dir)? {
        return Ok(false);
    }
    let owner: Option<EditorOwnerMetadata> =
        read_json_optional(&state_dir.join(EDITOR_OWNER_FILE))?;
    let ready: Option<EditorReadyMetadata> =
        read_json_optional(&state_dir.join(EDITOR_READY_FILE))?;
    Ok(matches!(
        (owner, ready),
        (Some(owner), Some(ready)) if owner.process_id == ready.process_id
    ))
}

/// Starts an Editor for `path`, or activates its existing Editor.
///
/// # Errors
///
/// Returns project/compatibility failures, lifecycle-state I/O failures, or
/// an executable/spawn failure.
pub fn launch_or_activate_editor(path: &Path) -> Result<EditorLaunch, LifecycleError> {
    let project = inspect_project(path)?;
    let state_dir = project_state_dir(&project);
    if editor_lock_is_held(&state_dir)? {
        write_control_request(&state_dir.join(EDITOR_ACTIVATE_FILE))?;
        return Ok(EditorLaunch {
            outcome: EditorLaunchOutcome::Activated,
            canonical_project: project.path().to_path_buf(),
        });
    }

    let editor = sibling_executable("engine-editor")?;
    Command::new(&editor)
        .arg("--project")
        .arg(project.path())
        .spawn()
        .map_err(|source| LifecycleError::Io {
            path: editor,
            source,
        })?;
    Ok(EditorLaunch {
        outcome: EditorLaunchOutcome::Spawned,
        canonical_project: project.path().to_path_buf(),
    })
}

/// Requests lifecycle-driven closure of an Editor after a replacement is ready.
///
/// # Errors
///
/// Returns an error when the project or control state cannot be resolved.
pub fn request_editor_close(path: &Path) -> Result<(), LifecycleError> {
    let project = inspect_project(path)?;
    write_control_request(&project_state_dir(&project).join(EDITOR_CLOSE_FILE))
}

/// Acquires the user-session Launcher lease.
///
/// When another Launcher is active this forwards the request to it and returns
/// `Ok(None)`. The primary Launcher receives `initial_switch_from` as its first
/// request and remains alive until its window closes.
///
/// # Errors
///
/// Returns an error when lifecycle state cannot be created or locked.
pub fn acquire_launcher(
    initial_switch_from: Option<PathBuf>,
) -> Result<Option<LauncherSession>, LifecycleError> {
    let root = state_root();
    fs::create_dir_all(&root).map_err(|source| LifecycleError::Io {
        path: root.clone(),
        source,
    })?;
    let lock_path = root.join(LAUNCHER_LOCK_FILE);
    let lock = open_lock_file(&lock_path)?;
    match lock.try_lock() {
        Ok(()) => {
            let request_path = root.join(LAUNCHER_REQUEST_FILE);
            let _ = fs::remove_file(&request_path);
            Ok(Some(LauncherSession {
                _lock: lock,
                request_path,
                last_request: None,
                initial_request: initial_switch_from.map(new_launcher_request),
            }))
        }
        Err(TryLockError::WouldBlock) => {
            write_launcher_request(initial_switch_from)?;
            Ok(None)
        }
        Err(TryLockError::Error(source)) => Err(LifecycleError::Io {
            path: lock_path,
            source,
        }),
    }
}

/// Activates the existing Launcher or starts its sibling executable.
///
/// `switch_from` is carried only as application lifecycle state; it is never
/// written to the project.
///
/// # Errors
///
/// Returns a lifecycle-state or process-spawn error.
pub fn activate_launcher(switch_from: Option<&Path>) -> Result<(), LifecycleError> {
    if launcher_is_running()? {
        return write_launcher_request(switch_from.map(Path::to_path_buf));
    }
    let launcher = sibling_executable("engine-launcher")?;
    let mut command = Command::new(&launcher);
    if let Some(path) = switch_from {
        command.arg("--switch-from").arg(path);
    }
    command.spawn().map_err(|source| LifecycleError::Io {
        path: launcher,
        source,
    })?;
    Ok(())
}

fn validate_project_name(name: &str) -> Result<(), LifecycleError> {
    let trimmed = name.trim();
    let path = Path::new(trimmed);
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || path.components().count() != 1
        || path.is_absolute()
    {
        return Err(LifecycleError::InvalidProjectName(name.to_owned()));
    }
    Ok(())
}

fn state_root() -> PathBuf {
    std::env::temp_dir().join("gameengine-project-lifecycle")
}

fn project_state_dir(project: &ProjectRoot) -> PathBuf {
    state_root().join(format!("{:016x}", location_hash(project.path())))
}

fn location_hash(path: &Path) -> u64 {
    #[cfg(windows)]
    let normalized = path.to_string_lossy().to_lowercase();
    #[cfg(not(windows))]
    let normalized = path.to_string_lossy().into_owned();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in normalized.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn open_lock_file(path: &Path) -> Result<File, LifecycleError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)
        .map_err(|source| LifecycleError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn editor_lock_is_held(state_dir: &Path) -> Result<bool, LifecycleError> {
    fs::create_dir_all(state_dir).map_err(|source| LifecycleError::Io {
        path: state_dir.to_path_buf(),
        source,
    })?;
    let lock_path = state_dir.join(EDITOR_LOCK_FILE);
    let lock = open_lock_file(&lock_path)?;
    match lock.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(source)) => Err(LifecycleError::Io {
            path: lock_path,
            source,
        }),
    }
}

fn launcher_is_running() -> Result<bool, LifecycleError> {
    let root = state_root();
    fs::create_dir_all(&root).map_err(|source| LifecycleError::Io {
        path: root.clone(),
        source,
    })?;
    let lock_path = root.join(LAUNCHER_LOCK_FILE);
    let lock = open_lock_file(&lock_path)?;
    match lock.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(source)) => Err(LifecycleError::Io {
            path: lock_path,
            source,
        }),
    }
}

fn write_control_request(path: &Path) -> Result<(), LifecycleError> {
    fs::write(path, request_nonce()).map_err(|source| LifecycleError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn take_control_request(path: &Path, last: &mut Option<String>) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    if last.as_deref() == Some(text.as_str()) {
        return false;
    }
    *last = Some(text);
    true
}

fn new_launcher_request(switch_from: PathBuf) -> LauncherRequest {
    LauncherRequest {
        switch_from: Some(switch_from),
        nonce: request_nonce(),
    }
}

fn write_launcher_request(switch_from: Option<PathBuf>) -> Result<(), LifecycleError> {
    let request = LauncherRequest {
        switch_from,
        nonce: request_nonce(),
    };
    write_json(&state_root().join(LAUNCHER_REQUEST_FILE), &request)
}

fn request_nonce() -> String {
    format!("{}-{}", std::process::id(), unix_millis())
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), LifecycleError> {
    let text = serde_json::to_string_pretty(value)?;
    fs::write(path, text).map_err(|source| LifecycleError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_json_optional<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Option<T>, LifecycleError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(LifecycleError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    Ok(Some(serde_json::from_str(&text)?))
}

fn remove_owned_metadata(path: &Path, process_id: u32) {
    let Ok(Some(metadata)) = read_json_optional::<EditorOwnerMetadata>(path) else {
        return;
    };
    if metadata.process_id == process_id {
        let _ = fs::remove_file(path);
    }
}

fn remove_owned_ready(path: &Path, process_id: u32) {
    let Ok(Some(metadata)) = read_json_optional::<EditorReadyMetadata>(path) else {
        return;
    };
    if metadata.process_id == process_id {
        let _ = fs::remove_file(path);
    }
}

fn sibling_executable(name: &str) -> Result<PathBuf, LifecycleError> {
    let current = std::env::current_exe().map_err(|source| LifecycleError::Io {
        path: PathBuf::from("<current executable>"),
        source,
    })?;
    let parent = current.parent().ok_or_else(|| {
        LifecycleError::ExecutableNotFound(PathBuf::from(name))
    })?;
    let path = parent.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if !path.is_file() {
        return Err(LifecycleError::ExecutableNotFound(path));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_scaffold_is_current_and_editor_openable() {
        let parent = tempfile::tempdir().expect("temp directory must be created");
        let final_path = parent.path().join("SampleGame");
        let project =
            create_standard_project(&final_path, "SampleGame").expect("scaffold must succeed");

        assert_eq!(project.config().schema_version, PROJECT_SCHEMA_VERSION);
        assert_eq!(
            project.config().engine_version,
            CURRENT_ENGINE_ASSOCIATION
        );
        assert!(project.config().project_id.as_str().starts_with("project_"));
        assert!(project.scenes_dir().join("main.scene.json").is_file());
        assert!(project.path().join("project_settings.json").is_file());
        assert!(project.game_dir().join("Cargo.toml").is_file());
    }

    #[test]
    fn editor_lease_is_exclusive_for_one_canonical_location() {
        let parent = tempfile::tempdir().expect("temp directory must be created");
        let final_path = parent.path().join("LeaseGame");
        create_standard_project(&final_path, "LeaseGame").expect("scaffold must succeed");

        let first = acquire_editor_project(&final_path).expect("first lease must succeed");
        let second = acquire_editor_project(&final_path).expect_err("second lease must fail");
        assert!(matches!(second, LifecycleError::EditorAlreadyOpen(_)));
        drop(first);
        acquire_editor_project(&final_path).expect("lease must be released on drop");
    }

    #[test]
    fn logical_project_id_survives_directory_move() {
        let parent = tempfile::tempdir().expect("temp directory must be created");
        let original = parent.path().join("Original");
        let moved = parent.path().join("Moved");
        let project = create_standard_project(&original, "Original").expect("scaffold must succeed");
        let id = project.config().project_id.clone();
        drop(project);
        fs::rename(&original, &moved).expect("project move must succeed");
        let reopened = inspect_project(&moved).expect("moved project must open");
        assert_eq!(reopened.config().project_id, id);
    }
}
