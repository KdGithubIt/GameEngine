//! Persistent editor preferences.

use engine_authoring::ProjectRoot;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const RUSTROVER_SOURCE_OPEN_DELAY: Duration = Duration::from_millis(1_200);

/// Editor-local viewport selected while Play Mode is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayModeView {
    /// The game-owned camera and target-screen UI.
    #[default]
    Game,
    /// The live runtime world observed through the editor camera.
    Scene,
}

/// Editor-local persistent preferences stored in the platform data directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorPreferences {
    /// Authoring documents open as workspace tabs, in tab order.
    ///
    /// Reopening the same project restores this whole tab set. Paths outside
    /// the restored project root are ignored, so switching projects cannot
    /// resurrect another project's tabs.
    #[serde(default)]
    pub open_documents: Vec<PathBuf>,
    /// Active workspace tab inside [`Self::open_documents`].
    ///
    /// Preferences written before tab restore existed only carry this field;
    /// such a project restores exactly this one document.
    #[serde(default)]
    pub last_document: Option<PathBuf>,
    /// Asset folder selected below the physical `assets/` root.
    #[serde(default)]
    pub selected_asset_folder: PathBuf,
    /// External editor executable. `None` uses the OS association only after
    /// an explicit Open Script action.
    #[serde(default)]
    pub external_editor: Option<PathBuf>,
    /// Argument template supporting `{path}`, `{line}`, and `{column}`.
    #[serde(default = "default_editor_arguments")]
    pub external_editor_arguments: String,
    /// UI preview preset restored with the last editor session.
    #[serde(default)]
    pub ui_preview_preset: usize,
    /// Stable scene entity IDs restored when the same document is reopened.
    #[serde(default)]
    pub selected_entity_ids: Vec<String>,
    /// Whether the shared bottom dock was expanded.
    #[serde(default = "default_true")]
    pub bottom_panel_open: bool,
    /// Stable editor-local name of the selected bottom panel.
    #[serde(default)]
    pub bottom_panel_tab: String,
    /// Warning and Info diagnostic codes hidden from the Problems surface.
    ///
    /// This is editor-local presentation state rather than project authoring
    /// data, so teams do not accidentally share one person's suppressions.
    #[serde(default)]
    pub suppressed_problem_codes: Vec<String>,
    /// Inspector card open state per component type, for the components whose
    /// state the author changed from the declared default.
    ///
    /// Only overrides are stored, so a component that later changes its
    /// declared default follows the new default for everyone who never
    /// touched its card.
    #[serde(default)]
    pub component_card_open: BTreeMap<String, bool>,
    /// Central viewport restored when the next Play session starts.
    #[serde(default)]
    pub play_mode_view: PlayModeView,
    /// User-local storage location selected from ProjectId + canonical root.
    #[serde(skip)]
    storage_path: Option<PathBuf>,
}

impl Default for EditorPreferences {
    fn default() -> Self {
        Self {
            open_documents: Vec::new(),
            last_document: None,
            selected_asset_folder: PathBuf::new(),
            external_editor: None,
            external_editor_arguments: default_editor_arguments(),
            ui_preview_preset: 0,
            selected_entity_ids: Vec::new(),
            bottom_panel_open: true,
            bottom_panel_tab: "assets".to_owned(),
            suppressed_problem_codes: Vec::new(),
            component_card_open: BTreeMap::new(),
            play_mode_view: PlayModeView::Game,
            storage_path: None,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_editor_arguments() -> String {
    "{path}:{line}:{column}".to_owned()
}

impl EditorPreferences {
    /// Loads preferences from the platform data directory.
    ///
    /// Returns `default` preferences if the file is missing or unparseable.
    pub fn load_for(project: &ProjectRoot) -> Self {
        let Some(path) = prefs_path(project) else {
            return Self::default();
        };
        let data = match std::fs::read(&path) {
            Ok(data) => data,
            Err(_) => {
                let mut preferences = Self::default();
                preferences.storage_path = Some(path);
                return preferences;
            }
        };
        let Ok(mut preferences) = serde_json::from_slice::<Self>(&data) else {
            let mut preferences = Self::default();
            preferences.storage_path = Some(path);
            return preferences;
        };
        preferences.storage_path = Some(path);
        preferences
    }

    /// Persists preferences to the platform data directory.
    ///
    /// Silently ignores I/O failures so a missing config directory does not
    /// crash the editor.
    pub fn save(&self) {
        let Some(path) = self.storage_path.as_ref() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, text);
        }
    }

    /// Launches the configured editor at a source location.
    ///
    /// This function is only called from explicit user actions. When no
    /// executable is configured it uses the OS association once.
    ///
    /// RustRover opens an arbitrary source file in LightEdit when its Cargo
    /// project is not already loaded, which disables project code insight and
    /// navigation. Project Rust sources therefore launch their generated
    /// `game/` Cargo host first and forward the requested source location after
    /// the IDE process has had time to register its command-line listener.
    pub fn open_source(&self, path: &Path, line: usize, column: usize) -> Result<(), String> {
        let Some(executable) = self.external_editor.as_ref() else {
            return open::that(path).map_err(|error| error.to_string());
        };
        if is_rustrover_executable(executable)
            && let Some(game_dir) = game_project_dir_for_source(path) {
                return open_rustrover_source(executable, &game_dir, path, line, column);
            }
        let rendered_path = path.to_string_lossy();
        let arguments = split_arguments(&self.external_editor_arguments)
            .into_iter()
            .map(|argument| {
                argument
                    .replace("{path}", &rendered_path)
                    .replace("{line}", &line.max(1).to_string())
                    .replace("{column}", &column.max(1).to_string())
            })
            .collect::<Vec<_>>();
        Command::new(executable)
            .args(arguments)
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

fn is_rustrover_executable(executable: &Path) -> bool {
    executable
        .file_stem()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.to_ascii_lowercase().contains("rustrover"))
}

fn game_project_dir_for_source(path: &Path) -> Option<PathBuf> {
    let project_root = project_root_for_rust_source(path)?;
    let game_dir = project_root.join("game");
    game_dir.join("Cargo.toml").is_file().then_some(game_dir)
}

fn project_root_for_rust_source(path: &Path) -> Option<&Path> {
    let mut current = path.parent();
    while let Some(directory) = current {
        if directory.file_name() == Some(OsStr::new("rust")) {
            let scripts = directory.parent()?;
            let assets = scripts.parent()?;
            if scripts.file_name() == Some(OsStr::new("scripts"))
                && assets.file_name() == Some(OsStr::new("assets"))
            {
                return assets.parent();
            }
        }
        current = directory.parent();
    }
    None
}

fn open_rustrover_source(
    executable: &Path,
    game_dir: &Path,
    source: &Path,
    line: usize,
    column: usize,
) -> Result<(), String> {
    Command::new(executable)
        .arg(game_dir)
        .spawn()
        .map_err(|error| error.to_string())?;

    let executable = executable.to_path_buf();
    let source = source.to_path_buf();
    let line = line.max(1).to_string();
    let column = column.max(1).to_string();
    std::thread::Builder::new()
        .name("rustrover-source-open".to_owned())
        .spawn(move || {
            std::thread::sleep(RUSTROVER_SOURCE_OPEN_DELAY);
            if let Err(error) = Command::new(&executable)
                .arg("--line")
                .arg(line)
                .arg("--column")
                .arg(column)
                .arg(&source)
                .spawn()
            {
                eprintln!(
                    "[editor.external_editor_followup_failed] could not open {} in RustRover: {error}",
                    source.display()
                );
            }
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn split_arguments(template: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in template.chars() {
        match character {
            '"' => quoted = !quoted,
            character if character.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    arguments.push(std::mem::take(&mut current));
                }
            }
            character => current.push(character),
        }
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    arguments
}

fn prefs_path(project: &ProjectRoot) -> Option<PathBuf> {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    #[cfg(windows)]
    let normalized = project.path().to_string_lossy().to_lowercase();
    #[cfg(not(windows))]
    let normalized = project.path().to_string_lossy().into_owned();
    for byte in normalized.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    dirs::data_local_dir().map(|root| {
        root.join("engine_editor").join("workspaces").join(format!(
            "{}-{hash:016x}.json",
            project.config().project_id.as_str()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_argument_template_preserves_quoted_paths() {
        assert_eq!(
            split_arguments("--goto \"{path}:{line}:{column}\" --wait"),
            vec!["--goto", "{path}:{line}:{column}", "--wait"]
        );
    }

    #[test]
    fn legacy_preferences_receive_editor_defaults() {
        let preferences: EditorPreferences =
            serde_json::from_str(r#"{"recent_projects":[]}"#).unwrap();
        assert_eq!(
            preferences.external_editor_arguments,
            "{path}:{line}:{column}"
        );
        assert!(preferences.suppressed_problem_codes.is_empty());
    }

    #[test]
    fn rustrover_executable_is_detected_case_insensitively() {
        let windows_executable = Path::new("RustRover64.exe");
        let unix_executable = Path::new("rustrover");
        let other_editor = Path::new("code.exe");
        assert!(is_rustrover_executable(windows_executable));
        assert!(is_rustrover_executable(unix_executable));
        assert!(!is_rustrover_executable(other_editor));
    }

    #[test]
    fn project_root_is_inferred_from_project_rust_source() {
        let source = Path::new("sample/assets/scripts/rust/systems/move_system.rs");
        let project_root = project_root_for_rust_source(source);
        assert_eq!(project_root, Some(Path::new("sample")));
    }

    #[test]
    fn unrelated_rust_source_has_no_game_project_root() {
        let source = Path::new("sample/src/main.rs");
        let project_root = project_root_for_rust_source(source);
        assert_eq!(project_root, None);
    }
}
