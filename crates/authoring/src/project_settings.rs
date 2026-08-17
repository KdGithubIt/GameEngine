//! Project-wide settings document (Phase 34 / ADR 0031).
//!
//! Stored as `project_settings.json` at the project root (alongside
//! `project.json`). When the file is absent all settings use their declared
//! defaults; the settings document is optional in the current project format.

use crate::persist::replace_file_contents;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

/// Schema version written to every `project_settings.json`.
pub const PROJECT_SETTINGS_SCHEMA_VERSION: u32 = 1;

const SETTINGS_JSON: &str = "project_settings.json";

// ---------------------------------------------------------------------------
// Sub-types
// ---------------------------------------------------------------------------

/// A render / collision layer slot (ADR 0031).
///
/// Up to 32 layers are allowed (indices 0–31).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    /// Zero-based layer index (0–31).
    pub index: u32,
    /// Human-readable layer name.
    pub name: String,
}

/// Binds a physical gamepad axis index to a logical action (Phase 43).
///
/// Values whose absolute magnitude falls below `deadzone` are treated as zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AxisBinding {
    /// Axis index reported by the platform driver.
    pub axis: u32,
    /// Minimum absolute value before the axis registers movement.
    pub deadzone: f32,
    /// Multiplier applied after deadzone filtering.
    pub scale: f32,
    /// Whether the physical axis direction is reversed before scaling.
    pub invert: bool,
}

/// Composes keyboard keys into one signed scalar/vector component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyAxisBinding {
    /// Zero selects X/scalar and one selects Y in the resolved vector.
    pub vector_component: u8,
    /// Keys contributing `-1` while held.
    pub negative_keys: Vec<String>,
    /// Keys contributing `+1` while held.
    pub positive_keys: Vec<String>,
    /// Multiplier applied to the composed signed value.
    pub scale: f32,
}

/// A logical input action mapped to one or more key codes (ADR 0031).
///
/// The `keys` field stores key identifiers using the Web KeyboardEvent code
/// convention (e.g., `"KeyW"`, `"ArrowUp"`).
/// Gamepad bindings are stored in `gamepad_buttons` (button indices) and
/// `gamepad_axes` (axis bindings with deadzone).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputAction {
    /// Logical action name used by game systems (e.g., `"move_forward"`).
    pub name: String,
    /// Key codes that trigger this action.
    pub keys: Vec<String>,
    /// Named mouse buttons that activate this action.
    pub mouse_buttons: Vec<String>,
    /// Gamepad button indices that trigger this action (Phase 43).
    pub gamepad_buttons: Vec<u32>,
    /// Gamepad axis bindings for this action (Phase 43).
    pub gamepad_axes: Vec<AxisBinding>,
    /// Keyboard positive/negative pairs used for scalar or vector actions.
    pub key_axes: Vec<KeyAxisBinding>,
}

// ---------------------------------------------------------------------------
// ProjectSettings
// ---------------------------------------------------------------------------

/// Versioned project-wide settings (ADR 0031).
///
/// Load from `project_settings.json` with [`ProjectSettings::load`] and save
/// with [`ProjectSettings::save`]. When the file is absent,
/// [`ProjectSettings::default`] provides sensible defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSettings {
    /// Current schema version for this settings document.
    pub schema_version: u32,
    /// Project tags — named strings that can be attached to entities.
    pub tags: Vec<String>,
    /// Named render/collision layer slots.
    pub layers: Vec<Layer>,
    /// Logical input actions and their key bindings.
    pub input_actions: Vec<InputAction>,
    /// Relative path to the start scene (from `assets/`), or `None`.
    ///
    /// Shared by Play, Build, and Validation (Phase 30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_scene: Option<String>,
    /// Project-wide preferred ECS system order and enabled state.
    #[serde(default, skip_serializing_if = "SystemSettings::is_default")]
    pub system_settings: SystemSettings,
    /// Native 2D defaults and stable sorting-layer identities (ADR 0127).
    #[serde(default)]
    pub native_2d: crate::native_2d::Project2dSettings,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            schema_version: PROJECT_SETTINGS_SCHEMA_VERSION,
            tags: Vec::new(),
            layers: vec![Layer {
                index: 0,
                name: "Default".into(),
            }],
            input_actions: default_input_actions(),
            start_scene: None,
            system_settings: SystemSettings::default(),
            native_2d: crate::native_2d::Project2dSettings::default(),
        }
    }
}

/// Schema version for the nested ECS system settings document.
pub const SYSTEM_SETTINGS_SCHEMA_VERSION: u32 = 1;

/// Identifies one persisted ECS schedule without depending on runtime types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSystemSchedule {
    /// Per-frame update systems.
    Update,
    /// Fixed-timestep systems.
    FixedUpdate,
}

/// Persisted preferences for one runtime schedule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemScheduleSettings {
    /// Preferred stable system ID order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
    /// Stable IDs that remain registered but do not execute.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled: Vec<String>,
}

/// Versioned project-wide ECS system settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemSettings {
    /// Nested schema version, independent from unrelated project settings.
    pub schema_version: u32,
    /// Per-frame schedule preferences.
    pub update: SystemScheduleSettings,
    /// Fixed-timestep schedule preferences.
    pub fixed_update: SystemScheduleSettings,
}

impl Default for SystemSettings {
    fn default() -> Self {
        Self {
            schema_version: SYSTEM_SETTINGS_SCHEMA_VERSION,
            update: SystemScheduleSettings::default(),
            fixed_update: SystemScheduleSettings::default(),
        }
    }
}

impl SystemSettings {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// Returns settings for one schedule.
    pub fn schedule(&self, schedule: ProjectSystemSchedule) -> &SystemScheduleSettings {
        match schedule {
            ProjectSystemSchedule::Update => &self.update,
            ProjectSystemSchedule::FixedUpdate => &self.fixed_update,
        }
    }

    /// Returns mutable settings for one schedule.
    pub fn schedule_mut(&mut self, schedule: ProjectSystemSchedule) -> &mut SystemScheduleSettings {
        match schedule {
            ProjectSystemSchedule::Update => &mut self.update,
            ProjectSystemSchedule::FixedUpdate => &mut self.fixed_update,
        }
    }

    /// Applies one shared authoring operation to the persisted preference data.
    pub fn apply(&mut self, command: SystemSettingsCommand) {
        match command {
            SystemSettingsCommand::SetOrder { schedule, order } => {
                self.schedule_mut(schedule).order = order;
            }
            SystemSettingsCommand::SetEnabled {
                schedule,
                system_id,
                is_enabled,
            } => {
                let disabled = &mut self.schedule_mut(schedule).disabled;
                disabled.retain(|id| id != &system_id);
                if !is_enabled {
                    disabled.push(system_id);
                }
            }
            SystemSettingsCommand::Reset { schedule } => {
                *self.schedule_mut(schedule) = SystemScheduleSettings::default();
            }
        }
    }
}

/// Command semantics shared by editor and future non-GUI authoring clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemSettingsCommand {
    /// Replaces the preferred order for one schedule.
    SetOrder {
        /// Target schedule.
        schedule: ProjectSystemSchedule,
        /// Canonical stable system IDs.
        order: Vec<String>,
    },
    /// Changes one system's enabled state without removing its order entry.
    SetEnabled {
        /// Target schedule.
        schedule: ProjectSystemSchedule,
        /// Canonical stable system ID.
        system_id: String,
        /// Whether the runtime should execute this entry.
        is_enabled: bool,
    },
    /// Clears saved order and disabled IDs for one schedule.
    Reset {
        /// Target schedule.
        schedule: ProjectSystemSchedule,
    },
}

/// Returns the built-in WASD input action bindings used when no
/// `project_settings.json` is present.
pub fn default_input_actions() -> Vec<InputAction> {
    vec![
        InputAction {
            name: "move_forward".into(),
            keys: vec!["KeyW".into()],
            mouse_buttons: Vec::new(),
            gamepad_buttons: Vec::new(),
            gamepad_axes: Vec::new(),
            key_axes: Vec::new(),
        },
        InputAction {
            name: "move_back".into(),
            keys: vec!["KeyS".into()],
            mouse_buttons: Vec::new(),
            gamepad_buttons: Vec::new(),
            gamepad_axes: Vec::new(),
            key_axes: Vec::new(),
        },
        InputAction {
            name: "move_left".into(),
            keys: vec!["KeyA".into()],
            mouse_buttons: Vec::new(),
            gamepad_buttons: Vec::new(),
            gamepad_axes: Vec::new(),
            key_axes: Vec::new(),
        },
        InputAction {
            name: "move_right".into(),
            keys: vec!["KeyD".into()],
            mouse_buttons: Vec::new(),
            gamepad_buttons: Vec::new(),
            gamepad_axes: Vec::new(),
            key_axes: Vec::new(),
        },
    ]
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Describes why a [`ProjectSettings`] operation failed.
#[derive(Debug)]
pub enum ProjectSettingsError {
    /// The JSON could not be parsed.
    Json(serde_json::Error),
    /// The file uses a schema version different from the current version.
    UnsupportedVersion {
        /// The version number found in the file.
        found: u32,
    },
    /// The nested ECS scheduling document uses a non-current schema version.
    UnsupportedSystemSettingsVersion {
        /// The nested version number found in the file.
        found: u32,
    },
    /// A layer entry carries an index greater than the maximum of 31.
    InvalidLayerIndex {
        /// The out-of-range index value.
        index: u32,
    },
    /// An I/O error occurred.
    Io(std::io::Error),
}

impl fmt::Display for ProjectSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "project settings JSON error: {e}"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "project_settings.json schema_version {found} is not supported \
                 (expected: {PROJECT_SETTINGS_SCHEMA_VERSION})"
            ),
            Self::UnsupportedSystemSettingsVersion { found } => write!(
                f,
                "project_settings.json system_settings.schema_version {found} is not supported \
                 (expected: {SYSTEM_SETTINGS_SCHEMA_VERSION})"
            ),
            Self::InvalidLayerIndex { index } => {
                write!(f, "layer index {index} exceeds the maximum of 31")
            }
            Self::Io(e) => write!(f, "project settings I/O error: {e}"),
        }
    }
}

impl std::error::Error for ProjectSettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::UnsupportedVersion { .. }
            | Self::UnsupportedSystemSettingsVersion { .. }
            | Self::InvalidLayerIndex { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Load / save
// ---------------------------------------------------------------------------

impl ProjectSettings {
    /// Loads `project_settings.json` from `project_root`.
    ///
    /// Returns `Ok(ProjectSettings::default())` when the file does not exist;
    /// absence of the settings document is part of the current project format.
    ///
    /// # Errors
    ///
    /// - [`ProjectSettingsError::Io`] for I/O failures other than file-not-found.
    /// - [`ProjectSettingsError::Json`] when the file contains invalid JSON.
    /// - [`ProjectSettingsError::UnsupportedVersion`] when `schema_version`
    ///   does not equal [`PROJECT_SETTINGS_SCHEMA_VERSION`].
    /// - [`ProjectSettingsError::UnsupportedSystemSettingsVersion`] when the
    ///   nested scheduling document does not use the current version.
    pub fn load(project_root: &Path) -> Result<Self, ProjectSettingsError> {
        let path = project_root.join(SETTINGS_JSON);
        if !path.exists() {
            return Ok(Self::default());
        }
        let json = std::fs::read_to_string(&path).map_err(ProjectSettingsError::Io)?;
        let settings: ProjectSettings =
            serde_json::from_str(&json).map_err(ProjectSettingsError::Json)?;
        if settings.schema_version != PROJECT_SETTINGS_SCHEMA_VERSION {
            return Err(ProjectSettingsError::UnsupportedVersion {
                found: settings.schema_version,
            });
        }
        for layer in &settings.layers {
            if layer.index > 31 {
                return Err(ProjectSettingsError::InvalidLayerIndex { index: layer.index });
            }
        }
        if settings.system_settings.schema_version != SYSTEM_SETTINGS_SCHEMA_VERSION {
            return Err(ProjectSettingsError::UnsupportedSystemSettingsVersion {
                found: settings.system_settings.schema_version,
            });
        }
        Ok(settings)
    }

    /// Saves this settings document to `project_settings.json` in
    /// `project_root`.
    ///
    /// # Errors
    ///
    /// - [`ProjectSettingsError::Json`] when serialization fails.
    /// - [`ProjectSettingsError::Io`] when the file cannot be written.
    pub fn save(&self, project_root: &Path) -> Result<(), ProjectSettingsError> {
        let path = project_root.join(SETTINGS_JSON);
        let json = serde_json::to_string_pretty(self).map_err(ProjectSettingsError::Json)?;
        replace_file_contents(&path, &json)
            .map_err(|e| ProjectSettingsError::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    }

    /// Returns the keys bound to a named action, if any.
    ///
    /// Returns an empty slice when the action name is not registered.
    pub fn keys_for_action(&self, action: &str) -> &[String] {
        self.input_actions
            .iter()
            .find(|a| a.name == action)
            .map(|a| a.keys.as_slice())
            .unwrap_or(&[])
    }

    /// Returns the gamepad button indices bound to a named action (Phase 43).
    ///
    /// Returns an empty slice when the action is not registered or has no gamepad bindings.
    pub fn gamepad_buttons_for_action(&self, action: &str) -> &[u32] {
        self.input_actions
            .iter()
            .find(|a| a.name == action)
            .map(|a| a.gamepad_buttons.as_slice())
            .unwrap_or(&[])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_have_one_default_layer() {
        let settings = ProjectSettings::default();
        assert_eq!(settings.layers.len(), 1);
        assert_eq!(settings.layers[0].name, "Default");
    }

    #[test]
    fn default_settings_have_wasd_actions() {
        let settings = ProjectSettings::default();
        assert!(
            settings
                .keys_for_action("move_forward")
                .contains(&"KeyW".to_string()),
            "move_forward must be bound to KeyW by default"
        );
        assert!(
            settings
                .keys_for_action("move_back")
                .contains(&"KeyS".to_string()),
            "move_back must be bound to KeyS by default"
        );
    }

    #[test]
    fn settings_roundtrip_preserves_all_fields() {
        let settings = ProjectSettings {
            schema_version: PROJECT_SETTINGS_SCHEMA_VERSION,
            tags: vec!["Player".into(), "Enemy".into()],
            layers: vec![
                Layer {
                    index: 0,
                    name: "Default".into(),
                },
                Layer {
                    index: 1,
                    name: "UI".into(),
                },
            ],
            input_actions: vec![InputAction {
                name: "jump".into(),
                keys: vec!["Space".into()],
                mouse_buttons: Vec::new(),
                gamepad_buttons: vec![0],
                gamepad_axes: Vec::new(),
                key_axes: Vec::new(),
            }],
            start_scene: Some("scenes/main.scene.json".into()),
            system_settings: SystemSettings::default(),
        };

        let json = serde_json::to_string_pretty(&settings).expect("must serialize");
        let parsed: ProjectSettings = serde_json::from_str(&json).expect("must parse");
        assert_eq!(parsed, settings);
    }

    #[test]
    fn missing_current_input_binding_fields_are_rejected() {
        let missing_action_fields = r#"{
          "schema_version": 1,
          "tags": [],
          "layers": [],
          "input_actions": [{
            "name": "move",
            "keys": ["KeyW"]
          }]
        }"#;
        assert!(serde_json::from_str::<ProjectSettings>(missing_action_fields).is_err());

        let missing_axis_fields = r#"{
          "schema_version": 1,
          "tags": [],
          "layers": [],
          "input_actions": [{
            "name": "move",
            "keys": ["KeyW"],
            "mouse_buttons": [],
            "gamepad_buttons": [],
            "gamepad_axes": [{ "axis": 1, "deadzone": 0.2 }],
            "key_axes": []
          }]
        }"#;
        assert!(serde_json::from_str::<ProjectSettings>(missing_axis_fields).is_err());
    }

    #[test]
    fn load_returns_defaults_when_file_absent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let settings = ProjectSettings::load(dir.path()).expect("must succeed");
        assert_eq!(settings, ProjectSettings::default());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut settings = ProjectSettings::default();
        settings.tags.push("Hero".into());
        settings.start_scene = Some("scenes/level1.scene.json".into());

        settings.save(dir.path()).expect("must save");
        let loaded = ProjectSettings::load(dir.path()).expect("must load");
        assert_eq!(loaded.tags, vec!["Hero".to_string()]);
        assert_eq!(
            loaded.start_scene.as_deref(),
            Some("scenes/level1.scene.json")
        );
    }

    #[test]
    fn load_rejects_future_schema_version() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("project_settings.json"),
            r#"{"schema_version":99,"tags":[],"layers":[],"input_actions":[]}"#,
        )
        .expect("write");
        let err = ProjectSettings::load(dir.path()).expect_err("must reject future version");
        assert!(
            matches!(err, ProjectSettingsError::UnsupportedVersion { found: 99 }),
            "expected UnsupportedVersion, got: {err}"
        );
    }

    #[test]
    fn keys_for_unknown_action_returns_empty_slice() {
        let settings = ProjectSettings::default();
        assert!(settings.keys_for_action("nonexistent_action").is_empty());
    }

    #[test]
    fn system_settings_commands_update_current_preferences() {
        let mut settings = ProjectSettings::default();
        assert_eq!(settings.system_settings, SystemSettings::default());

        settings
            .system_settings
            .apply(SystemSettingsCommand::SetOrder {
                schedule: ProjectSystemSchedule::Update,
                order: vec!["engine.b".into(), "engine.a".into()],
            });
        settings
            .system_settings
            .apply(SystemSettingsCommand::SetEnabled {
                schedule: ProjectSystemSchedule::Update,
                system_id: "engine.a".into(),
                is_enabled: false,
            });

        assert_eq!(
            settings.system_settings.update.order,
            ["engine.b", "engine.a"]
        );
        assert_eq!(settings.system_settings.update.disabled, ["engine.a"]);
    }

    #[test]
    fn load_reports_nested_system_settings_version_separately() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SETTINGS_JSON),
            r#"{
                "schema_version": 1,
                "tags": [],
                "layers": [],
                "input_actions": [],
                "system_settings": {
                    "schema_version": 99,
                    "update": {},
                    "fixed_update": {}
                }
            }"#,
        )
        .unwrap();

        assert!(matches!(
            ProjectSettings::load(dir.path()),
            Err(ProjectSettingsError::UnsupportedSystemSettingsVersion { found: 99 })
        ));
    }
}
