//! Launcher-owned user application state.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Maximum number of remembered Launcher projects.
pub const MAX_RECENT_PROJECTS: usize = 10;

/// User-private Launcher preferences stored outside canonical project data.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LauncherPreferences {
    /// Most-recently-used project locations, newest first.
    pub recent_projects: Vec<PathBuf>,
    /// Parent directory most recently used by the create-project flow.
    #[serde(default)]
    pub new_project_parent: Option<PathBuf>,
}

/// Failure to load the Launcher-owned user preference file.
#[derive(Debug)]
pub(crate) enum LauncherPreferencesLoadError {
    /// The platform did not provide a user-local application data directory.
    DataDirectoryUnavailable,
    /// The preference file could not be read.
    Io(io::Error),
    /// The preference file was not valid JSON for the current user-state schema.
    Json(serde_json::Error),
}

impl fmt::Display for LauncherPreferencesLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DataDirectoryUnavailable => {
                formatter.write_str("Launcher application-data directory is unavailable")
            }
            Self::Io(error) => write!(formatter, "could not read Launcher preferences: {error}"),
            Self::Json(error) => write!(formatter, "could not decode Launcher preferences: {error}"),
        }
    }
}

impl LauncherPreferences {
    /// Loads Launcher preferences, falling back to empty state when user state is unavailable.
    pub fn load() -> Self {
        Self::load_checked().unwrap_or_default()
    }

    /// Loads preferences while preserving failures for application-layer diagnostics.
    pub(crate) fn load_checked() -> Result<Self, LauncherPreferencesLoadError> {
        let path =
            preferences_path().ok_or(LauncherPreferencesLoadError::DataDirectoryUnavailable)?;
        Self::load_checked_from(&path)
    }

    /// Loads preferences from an explicit file path used by focused tests.
    pub(crate) fn load_checked_from(
        path: &Path,
    ) -> Result<Self, LauncherPreferencesLoadError> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(LauncherPreferencesLoadError::Io(error)),
        };
        let mut preferences = serde_json::from_slice::<Self>(&data)
            .map_err(LauncherPreferencesLoadError::Json)?;
        preferences.recent_projects.retain(|path| path.is_dir());
        if preferences
            .new_project_parent
            .as_ref()
            .is_some_and(|path| !path.is_dir())
        {
            preferences.new_project_parent = None;
        }
        Ok(preferences)
    }

    fn save(&self) {
        let Some(path) = preferences_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, text);
        }
    }

    /// Remembers the parent directory used by the create-project dialog.
    pub fn remember_new_project_parent(&mut self, path: &Path) {
        self.new_project_parent = Some(path.to_path_buf());
        self.save();
    }

    /// Moves `path` to the front of the recent list without touching disk.
    pub fn record_recent(&mut self, path: &Path) {
        self.recent_projects.retain(|candidate| candidate != path);
        self.recent_projects.insert(0, path.to_path_buf());
        self.recent_projects.truncate(MAX_RECENT_PROJECTS);
    }

    /// Drops `path` from the recent list without touching disk.
    pub fn forget_recent(&mut self, path: &Path) {
        self.recent_projects.retain(|candidate| candidate != path);
    }

    /// Records one recent project and persists the updated user state.
    pub fn push_recent(&mut self, path: &Path) {
        self.record_recent(path);
        self.save();
    }

    /// Removes one recent project and persists the updated user state.
    pub fn remove_recent(&mut self, path: &Path) {
        self.forget_recent(path);
        self.save();
    }
}

fn preferences_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|root| root.join("engine_launcher").join("preferences.json"))
}
