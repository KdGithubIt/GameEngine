//! Loopback-only remote control plane for Launcher-owned project lifecycle.
//!
//! This module is deliberately narrower than Remote AI Studio's AgentRun
//! surface. It can report Launcher/recent-project state and can activate or
//! start an Editor only for a project already remembered by the Launcher. It
//! never accepts filesystem locations, executable paths, argv, shell commands,
//! environment-variable payloads, or authoring operations from a remote client.

use crate::preferences::{LauncherPreferences, LauncherPreferencesLoadError};
use engine_project_lifecycle::{
    editor_is_ready, editor_owner_metadata, inspect_project, launch_or_activate_editor,
    launcher_is_active, project_location_key, EditorLaunchOutcome, LifecycleError,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// Environment variable containing the dedicated lifecycle-host bearer credential.
pub const REMOTE_LIFECYCLE_TOKEN_ENV: &str = "ENGINE_REMOTE_LIFECYCLE_TOKEN";
/// Default loopback port for the lifecycle host.
pub const DEFAULT_REMOTE_LIFECYCLE_PORT: u16 = 41718;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const EDITOR_BOOTSTRAP_LEASE_TIMEOUT: Duration = Duration::from_secs(10);
const EDITOR_BOOTSTRAP_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Configuration for the GUI-free remote lifecycle host.
pub struct RemoteLifecycleHostConfig {
    bearer_token: String,
    port: u16,
    allow_editor_start: bool,
}

impl RemoteLifecycleHostConfig {
    /// Creates a loopback host configuration with Editor startup disabled.
    pub fn new(bearer_token: impl Into<String>) -> Self {
        Self {
            bearer_token: bearer_token.into(),
            port: DEFAULT_REMOTE_LIFECYCLE_PORT,
            allow_editor_start: false,
        }
    }

    /// Overrides the loopback TCP port.
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Enables or disables the distinct remote Editor-start lifecycle permission.
    pub fn with_editor_start_permission(mut self, allowed: bool) -> Self {
        self.allow_editor_start = allowed;
        self
    }
}

/// Failure to configure or run the local lifecycle host process.
#[derive(Debug)]
pub enum RemoteLifecycleHostError {
    /// The configured credential is absent, weak, or unsafe for an HTTP header.
    InvalidCredential,
    /// The loopback listener could not be created.
    Bind(io::Error),
    /// Accepted connections could not be served.
    Serve(io::Error),
}

impl fmt::Display for RemoteLifecycleHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCredential => formatter.write_str(
                "remote lifecycle credential must contain at least 32 visible ASCII characters",
            ),
            Self::Bind(error) => write!(formatter, "could not bind remote lifecycle host: {error}"),
            Self::Serve(error) => write!(formatter, "remote lifecycle host failed: {error}"),
        }
    }
}

impl std::error::Error for RemoteLifecycleHostError {}

/// Runs the authenticated lifecycle host on a loopback-only TCP listener.
///
/// The call blocks for the lifetime of the host process. Remote clients can
/// inspect lifecycle state regardless of policy, but Editor activation/start is
/// rejected unless [`RemoteLifecycleHostConfig::with_editor_start_permission`]
/// explicitly enabled it.
///
/// # Errors
///
/// Returns an error when the credential is invalid or the loopback listener
/// cannot be created or served.
pub fn serve_remote_lifecycle_host(
    config: RemoteLifecycleHostConfig,
) -> Result<(), RemoteLifecycleHostError> {
    validate_bearer_token(&config.bearer_token)?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))
        .map_err(RemoteLifecycleHostError::Bind)?;
    let authority = RemoteLifecycleAuthority::new(config.allow_editor_start);
    for stream in listener.incoming() {
        let mut stream = stream.map_err(RemoteLifecycleHostError::Serve)?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(RemoteLifecycleHostError::Serve)?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(RemoteLifecycleHostError::Serve)?;
        let _ = handle_connection(&mut stream, &config.bearer_token, &authority);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LauncherAvailability {
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProjectAvailability {
    Stopped,
    Starting,
    Ready,
    Incompatible,
    Unavailable,
}

#[derive(Debug, Serialize)]
struct RemoteProjectSummary {
    project_key: String,
    name: Option<String>,
    availability: ProjectAvailability,
}

#[derive(Debug, Serialize)]
struct RemoteLifecycleSnapshot {
    host: &'static str,
    launcher: LauncherAvailability,
    editor_start_permitted: bool,
    projects: Vec<RemoteProjectSummary>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ActivationOutcome {
    Spawned,
    Activated,
}

#[derive(Debug, Serialize)]
struct RemoteEditorActivation {
    project_key: String,
    outcome: ActivationOutcome,
    availability: ProjectAvailability,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ErrorCategory {
    PermissionDenied,
    ProjectNotKnown,
    IncompatibleProject,
    ProjectUnavailable,
    EditorUnavailable,
    EditorBootstrapFailed,
    LifecycleUnavailable,
}

#[derive(Debug, Serialize)]
struct PublicLifecycleError {
    category: ErrorCategory,
    message: &'static str,
    retryable: bool,
}

impl PublicLifecycleError {
    fn permission_denied() -> Self {
        Self {
            category: ErrorCategory::PermissionDenied,
            message: "Remote Editor startup is not permitted by this lifecycle host.",
            retryable: false,
        }
    }

    fn project_not_known() -> Self {
        Self {
            category: ErrorCategory::ProjectNotKnown,
            message: "The selected project is not present in Launcher recent-project state.",
            retryable: false,
        }
    }

    fn lifecycle_unavailable() -> Self {
        Self {
            category: ErrorCategory::LifecycleUnavailable,
            message: "GameEngine lifecycle state is temporarily unavailable.",
            retryable: true,
        }
    }

    fn http_status(&self) -> u16 {
        match self.category {
            ErrorCategory::PermissionDenied => 403,
            ErrorCategory::ProjectNotKnown => 404,
            ErrorCategory::IncompatibleProject | ErrorCategory::ProjectUnavailable => 409,
            ErrorCategory::EditorUnavailable
            | ErrorCategory::EditorBootstrapFailed
            | ErrorCategory::LifecycleUnavailable => 503,
        }
    }
}

struct RemoteLifecycleAuthority {
    allow_editor_start: bool,
    preferences_path: Option<PathBuf>,
}

impl RemoteLifecycleAuthority {
    fn new(allow_editor_start: bool) -> Self {
        Self {
            allow_editor_start,
            preferences_path: None,
        }
    }

    #[cfg(test)]
    fn with_preferences_path(allow_editor_start: bool, preferences_path: PathBuf) -> Self {
        Self {
            allow_editor_start,
            preferences_path: Some(preferences_path),
        }
    }

    fn preferences(&self) -> Result<LauncherPreferences, PublicLifecycleError> {
        let result = if let Some(path) = self.preferences_path.as_deref() {
            LauncherPreferences::load_checked_from(path)
        } else {
            LauncherPreferences::load_checked()
        };
        result.map_err(map_preferences_error)
    }

    fn snapshot(&self) -> Result<RemoteLifecycleSnapshot, PublicLifecycleError> {
        let launcher = if launcher_is_active().map_err(map_status_error)? {
            LauncherAvailability::Running
        } else {
            LauncherAvailability::Stopped
        };
        let preferences = self.preferences()?;
        let mut seen = BTreeSet::new();
        let mut projects = Vec::new();
        for remembered in preferences.recent_projects {
            let Ok(canonical) = fs::canonicalize(&remembered) else {
                continue;
            };
            let key = project_location_key(&canonical).map_err(map_status_error)?;
            if !seen.insert(key.clone()) {
                continue;
            }
            projects.push(summarize_project(key, &canonical));
        }
        Ok(RemoteLifecycleSnapshot {
            host: "ready",
            launcher,
            editor_start_permitted: self.allow_editor_start,
            projects,
        })
    }

    fn activate_editor(
        &self,
        project_key: &str,
    ) -> Result<RemoteEditorActivation, PublicLifecycleError> {
        if !self.allow_editor_start {
            return Err(PublicLifecycleError::permission_denied());
        }
        if !valid_project_key(project_key) {
            return Err(PublicLifecycleError::project_not_known());
        }
        let project_path = self.resolve_known_project(project_key)?;
        inspect_project(&project_path).map_err(map_selection_error)?;
        let launch = launch_or_activate_editor(&project_path).map_err(map_launch_error)?;
        let (outcome, availability) = match launch.outcome {
            EditorLaunchOutcome::Spawned => (
                ActivationOutcome::Spawned,
                wait_for_editor_bootstrap(&project_path)?,
            ),
            EditorLaunchOutcome::Activated => (
                ActivationOutcome::Activated,
                editor_availability(&project_path).unwrap_or(ProjectAvailability::Starting),
            ),
        };
        Ok(RemoteEditorActivation {
            project_key: project_key.to_owned(),
            outcome,
            availability,
        })
    }

    fn resolve_known_project(&self, project_key: &str) -> Result<PathBuf, PublicLifecycleError> {
        let preferences = self.preferences()?;
        let mut matched: Option<PathBuf> = None;
        for remembered in preferences.recent_projects {
            let Ok(canonical) = fs::canonicalize(&remembered) else {
                continue;
            };
            let Ok(candidate_key) = project_location_key(&canonical) else {
                continue;
            };
            if candidate_key != project_key {
                continue;
            }
            match matched.as_ref() {
                None => matched = Some(canonical),
                Some(previous) if previous == &canonical => {}
                Some(_) => return Err(PublicLifecycleError::lifecycle_unavailable()),
            }
        }
        matched.ok_or_else(PublicLifecycleError::project_not_known)
    }
}

fn summarize_project(project_key: String, path: &Path) -> RemoteProjectSummary {
    match inspect_project(path) {
        Ok(project) => {
            let configured_name = project.config().name.trim();
            let name = if configured_name.is_empty() {
                None
            } else {
                Some(configured_name.to_owned())
            };
            RemoteProjectSummary {
                project_key,
                name,
                availability: editor_availability(path).unwrap_or(ProjectAvailability::Unavailable),
            }
        }
        Err(LifecycleError::IncompatibleEngine { .. }) => RemoteProjectSummary {
            project_key,
            name: None,
            availability: ProjectAvailability::Incompatible,
        },
        Err(_) => RemoteProjectSummary {
            project_key,
            name: None,
            availability: ProjectAvailability::Unavailable,
        },
    }
}

fn editor_availability(path: &Path) -> Result<ProjectAvailability, LifecycleError> {
    match editor_owner_metadata(path)? {
        None => Ok(ProjectAvailability::Stopped),
        Some(_) if editor_is_ready(path)? => Ok(ProjectAvailability::Ready),
        Some(_) => Ok(ProjectAvailability::Starting),
    }
}

fn wait_for_editor_bootstrap(path: &Path) -> Result<ProjectAvailability, PublicLifecycleError> {
    let deadline = Instant::now() + EDITOR_BOOTSTRAP_LEASE_TIMEOUT;
    loop {
        match editor_owner_metadata(path) {
            Ok(Some(_)) => {
                return match editor_is_ready(path) {
                    Ok(true) => Ok(ProjectAvailability::Ready),
                    Ok(false) => Ok(ProjectAvailability::Starting),
                    Err(_) => Ok(ProjectAvailability::Starting),
                };
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(EDITOR_BOOTSTRAP_POLL_INTERVAL),
            Ok(None) => {
                return Err(PublicLifecycleError {
                    category: ErrorCategory::EditorBootstrapFailed,
                    message: "The Editor process did not acquire the selected project lease.",
                    retryable: true,
                });
            }
            Err(error) => return Err(map_launch_error(error)),
        }
    }
}

fn valid_project_key(project_key: &str) -> bool {
    project_key.len() == 16
        && project_key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn map_preferences_error(_: LauncherPreferencesLoadError) -> PublicLifecycleError {
    PublicLifecycleError::lifecycle_unavailable()
}

fn map_status_error(_: LifecycleError) -> PublicLifecycleError {
    PublicLifecycleError::lifecycle_unavailable()
}

fn map_selection_error(error: LifecycleError) -> PublicLifecycleError {
    match error {
        LifecycleError::IncompatibleEngine { .. } => PublicLifecycleError {
            category: ErrorCategory::IncompatibleProject,
            message: "The selected project is not compatible with this GameEngine build.",
            retryable: false,
        },
        _ => PublicLifecycleError {
            category: ErrorCategory::ProjectUnavailable,
            message: "The selected Launcher project is invalid or unavailable.",
            retryable: false,
        },
    }
}

fn map_launch_error(error: LifecycleError) -> PublicLifecycleError {
    match error {
        LifecycleError::IncompatibleEngine { .. } => PublicLifecycleError {
            category: ErrorCategory::IncompatibleProject,
            message: "The selected project is not compatible with this GameEngine build.",
            retryable: false,
        },
        LifecycleError::ExecutableNotFound(_) => PublicLifecycleError {
            category: ErrorCategory::EditorUnavailable,
            message: "The GameEngine Editor executable is unavailable.",
            retryable: false,
        },
        LifecycleError::Project(_) => PublicLifecycleError {
            category: ErrorCategory::ProjectUnavailable,
            message: "The selected Launcher project is invalid or unavailable.",
            retryable: false,
        },
        LifecycleError::EditorAlreadyOpen(_) => PublicLifecycleError {
            category: ErrorCategory::EditorBootstrapFailed,
            message: "The Editor lifecycle changed while the activation request was being processed.",
            retryable: true,
        },
        LifecycleError::Io { .. }
        | LifecycleError::Json(_)
        | LifecycleError::Scaffold { .. }
        | LifecycleError::ProjectAlreadyExists(_)
        | LifecycleError::InvalidProjectName(_) => PublicLifecycleError {
            category: ErrorCategory::EditorBootstrapFailed,
            message: "The Editor could not be started for the selected project.",
            retryable: true,
        },
    }
}

fn validate_bearer_token(token: &str) -> Result<(), RemoteLifecycleHostError> {
    if token.len() < 32
        || !token
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(RemoteLifecycleHostError::InvalidCredential);
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn authorized(headers: &BTreeMap<String, String>, bearer_token: &str) -> bool {
    let Some(value) = headers.get("authorization") else {
        return false;
    };
    let Some(provided) = value.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(provided.as_bytes(), bearer_token.as_bytes())
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    content_length: usize,
}

fn read_http_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    let mut bytes = Vec::with_capacity(1024);
    let header_end = loop {
        if bytes.len() >= MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP headers exceed lifecycle-host limit",
            ));
        }
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "HTTP request ended before headers completed",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text = std::str::from_utf8(&bytes[..header_end]).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "HTTP headers are not UTF-8")
    })?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default();
    let path = request_parts.next().unwrap_or_default();
    let version = request_parts.next().unwrap_or_default();
    if method.is_empty()
        || path.is_empty()
        || request_parts.next().is_some()
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid HTTP request line",
        ));
    }
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid HTTP header",
            ));
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    if headers.contains_key("transfer-encoding") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "transfer encoding is not supported",
        ));
    }
    let content_length = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid content length"))?,
        None => 0,
    };
    Ok(HttpRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        headers,
        content_length,
    })
}

fn handle_connection(
    stream: &mut TcpStream,
    bearer_token: &str,
    authority: &RemoteLifecycleAuthority,
) -> io::Result<()> {
    let request = match read_http_request(stream) {
        Ok(request) => request,
        Err(_) => {
            return write_error(
                stream,
                400,
                "invalid_request",
                "Invalid lifecycle request.",
                false,
            );
        }
    };
    if request.content_length != 0 {
        return write_error(
            stream,
            400,
            "invalid_request",
            "Lifecycle requests do not accept request bodies.",
            false,
        );
    }
    if request.method == "GET" && request.path == "/health" {
        return write_json(stream, 200, &json!({ "status": "ready" }));
    }
    if !authorized(&request.headers, bearer_token) {
        return write_error(
            stream,
            401,
            "unauthorized",
            "Remote lifecycle authentication is required.",
            false,
        );
    }
    if request.method == "GET" && request.path == "/api/v1/lifecycle" {
        return match authority.snapshot() {
            Ok(snapshot) => write_serialized(stream, 200, &snapshot),
            Err(error) => write_public_error(stream, &error),
        };
    }
    if request.method == "POST" {
        if let Some(project_key) = activation_project_key(&request.path) {
            return match authority.activate_editor(project_key) {
                Ok(activation) => write_serialized(stream, 200, &activation),
                Err(error) => write_public_error(stream, &error),
            };
        }
    }
    write_error(
        stream,
        404,
        "not_found",
        "Lifecycle endpoint not found.",
        false,
    )
}

fn activation_project_key(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/v1/projects/")?;
    let project_key = rest.strip_suffix("/editor")?;
    if project_key.is_empty() || project_key.contains('/') {
        return None;
    }
    Some(project_key)
}

fn write_public_error(stream: &mut TcpStream, error: &PublicLifecycleError) -> io::Result<()> {
    write_json(stream, error.http_status(), &json!({ "error": error }))
}

fn write_error(
    stream: &mut TcpStream,
    status: u16,
    category: &str,
    message: &str,
    retryable: bool,
) -> io::Result<()> {
    write_json(
        stream,
        status,
        &json!({
            "error": {
                "category": category,
                "message": message,
                "retryable": retryable,
            }
        }),
    )
}

fn write_serialized<T: Serialize>(stream: &mut TcpStream, status: u16, value: &T) -> io::Result<()> {
    let value = serde_json::to_value(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_json(stream, status, &value)
}

fn write_json(stream: &mut TcpStream, status: u16, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        503 => "Service Unavailable",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project_lifecycle::{LifecycleError, acquire_editor_project, create_standard_project};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock must follow Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "gameengine-remote-lifecycle-{name}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("test directory must be created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_preferences(root: &Path, projects: Vec<PathBuf>) -> PathBuf {
        let path = root.join("preferences.json");
        let preferences = LauncherPreferences {
            recent_projects: projects,
            new_project_parent: None,
        };
        let text = serde_json::to_string(&preferences).expect("preferences must serialize");
        fs::write(&path, text).expect("preferences must be written");
        path
    }

    #[test]
    fn explicit_known_project_activation_reuses_existing_editor_lease() {
        let root = TestDir::new("lease-reuse");
        let project_path = root.path().join("alpha");
        create_standard_project(&project_path, "Alpha").expect("project must be created");
        let preferences_path = write_preferences(root.path(), vec![project_path.clone()]);
        let lease = acquire_editor_project(&project_path).expect("Editor lease must be acquired");
        let authority = RemoteLifecycleAuthority::with_preferences_path(true, preferences_path);
        let snapshot = authority.snapshot().expect("snapshot must succeed");
        let project_key = snapshot.projects[0].project_key.clone();

        let activation = authority
            .activate_editor(&project_key)
            .expect("known leased project must activate");

        assert_eq!(activation.outcome, ActivationOutcome::Activated);
        assert_eq!(activation.availability, ProjectAvailability::Starting);
        drop(lease);
    }

    #[test]
    fn editor_start_requires_distinct_lifecycle_permission() {
        let root = TestDir::new("permission");
        let project_path = root.path().join("alpha");
        create_standard_project(&project_path, "Alpha").expect("project must be created");
        let preferences_path = write_preferences(root.path(), vec![project_path]);
        let authority = RemoteLifecycleAuthority::with_preferences_path(false, preferences_path);
        let snapshot = authority.snapshot().expect("snapshot must succeed");
        let project_key = snapshot.projects[0].project_key.clone();

        let error = authority
            .activate_editor(&project_key)
            .expect_err("startup must remain permission-gated");

        assert_eq!(error.category, ErrorCategory::PermissionDenied);
    }

    #[test]
    fn unknown_project_key_cannot_select_an_arbitrary_filesystem_location() {
        let root = TestDir::new("unknown-key");
        let preferences_path = write_preferences(root.path(), Vec::new());
        let authority = RemoteLifecycleAuthority::with_preferences_path(true, preferences_path);

        let error = authority
            .activate_editor("0123456789abcdef")
            .expect_err("unknown project key must be rejected");

        assert_eq!(error.category, ErrorCategory::ProjectNotKnown);
    }

    #[test]
    fn remote_snapshot_does_not_expose_project_paths() {
        let root = TestDir::new("path-sanitization");
        let project_path = root.path().join("private-project-location");
        create_standard_project(&project_path, "VisibleName").expect("project must be created");
        let preferences_path = write_preferences(root.path(), vec![project_path.clone()]);
        let authority = RemoteLifecycleAuthority::with_preferences_path(false, preferences_path);

        let snapshot = authority.snapshot().expect("snapshot must succeed");
        let json = serde_json::to_string(&snapshot).expect("snapshot must serialize");

        assert!(json.contains("VisibleName"));
        assert!(!json.contains(project_path.to_string_lossy().as_ref()));
        assert!(!json.contains(root.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn lifecycle_errors_are_sanitized_before_remote_serialization() {
        let local_error = LifecycleError::Io {
            path: PathBuf::from("C:/Users/alice/private/project"),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "secret local detail"),
        };

        let public_error = map_launch_error(local_error);
        let json = serde_json::to_string(&public_error).expect("public error must serialize");

        assert_eq!(public_error.category, ErrorCategory::EditorBootstrapFailed);
        assert!(!json.contains("alice"));
        assert!(!json.contains("secret local detail"));
    }

    #[test]
    fn incompatible_project_errors_use_a_stable_sanitized_category() {
        let public_error = map_selection_error(LifecycleError::IncompatibleEngine {
            found: "old-engine".to_owned(),
            expected: "current-engine",
        });

        assert_eq!(public_error.category, ErrorCategory::IncompatibleProject);
        assert!(!public_error.message.contains("old-engine"));
    }

    #[test]
    fn generic_process_and_shell_routes_do_not_exist() {
        assert!(activation_project_key("/api/v1/process").is_none());
        assert!(activation_project_key("/api/v1/shell").is_none());
        assert!(activation_project_key("/api/v1/projects/0123456789abcdef/editor").is_some());
    }

    #[test]
    fn bearer_token_validation_rejects_short_or_header_unsafe_credentials() {
        assert!(validate_bearer_token("short").is_err());
        assert!(
            validate_bearer_token("0123456789abcdef0123456789abcde\n").is_err()
        );
        assert!(validate_bearer_token("0123456789abcdef0123456789abcdef").is_ok());
    }
}
