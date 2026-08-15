//! Save data model and slot storage (Phase 56 / ADR 0048).
//!
//! [`SaveData`] is a flat, schema-versioned key-value document that games use
//! to record progress (mission state, unlocks, settings); [`SaveStore`] owns
//! a directory of `slot_<n>.save.json` files and reads/writes them
//! atomically. The engine owns this format because saves are produced and
//! consumed by the runtime and packaged games, never by the authoring
//! pipeline (ADR 0048 §1).
//!
//! Script and project-Rust adapters never touch [`SaveStore`] directly; they
//! queue validated persistence requests that this scene-domain service applies.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};

use engine_authoring::PersistError;
use serde::{Deserialize, Serialize};

use engine_ecs::ResMut;

/// Schema version for `*.save.json` files.
pub const SAVE_SCHEMA_VERSION: u32 = 1;

/// Maximum pending persistence requests from project Rust callbacks.
pub const MAX_GAME_SAVE_COMMANDS: usize = 64;

// ---------------------------------------------------------------------------
// SaveValue
// ---------------------------------------------------------------------------

/// A single value stored under one key in a [`SaveData`] document.
///
/// Serializes untagged: a `Text` becomes a JSON string, a `Number` a JSON
/// number, and a `Flag` a JSON boolean, with no wrapper object. This mirrors
/// UI binding values and maps 1:1 onto the three scalar types exposed by
/// the high-level scripting adapter (ADR 0048 §1, §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SaveValue {
    /// A string value.
    Text(String),
    /// A numeric value.
    Number(f64),
    /// A boolean value.
    Flag(bool),
}

// ---------------------------------------------------------------------------
// SaveData
// ---------------------------------------------------------------------------

/// A schema-versioned, flat key-value save document (ADR 0048 §1).
///
/// `SaveData` is engine-owned game state, not authoring data: games define
/// their own key conventions (for example `"party.0.id"`) at their own
/// discretion. The active save lives in the world as a plain resource that
/// any system can read and mutate directly; Rhai scripts only ever reach it
/// through the queued-command path in the high-level scripting adapter
/// (ADR 0037, ADR 0048 §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveData {
    /// Schema version. Readers accept only [`SAVE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Current-format save entries. The canonical writer emits this map even when empty.
    entries: BTreeMap<String, SaveValue>,
}

impl SaveData {
    /// Creates an empty save document at [`SAVE_SCHEMA_VERSION`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the value stored under `key`, or `None` if it is unset.
    pub fn get(&self, key: &str) -> Option<&SaveValue> {
        self.entries.get(key)
    }

    /// Sets (or replaces) the value stored under `key`.
    pub fn set(&mut self, key: impl Into<String>, value: SaveValue) {
        self.entries.insert(key.into(), value);
    }

    /// Removes and returns the value stored under `key`, if any.
    pub fn remove(&mut self, key: &str) -> Option<SaveValue> {
        self.entries.remove(key)
    }

    /// Returns every stored key, in ascending order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Removes every stored key.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Parses a `*.save.json` document.
    ///
    /// # Errors
    ///
    /// - [`SaveDataError::Json`] for malformed JSON or a missing required field.
    /// - [`SaveDataError::UnsupportedVersion`] when `schema_version` does not equal
    ///   [`SAVE_SCHEMA_VERSION`].
    pub fn from_json_str(json: &str) -> Result<Self, SaveDataError> {
        let data: SaveData = serde_json::from_str(json).map_err(SaveDataError::Json)?;
        if data.schema_version != SAVE_SCHEMA_VERSION {
            return Err(SaveDataError::UnsupportedVersion {
                found: data.schema_version,
            });
        }
        Ok(data)
    }

    /// Serializes this document to canonical pretty-printed JSON.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if serialization fails.
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

impl Default for SaveData {
    fn default() -> Self {
        Self {
            schema_version: SAVE_SCHEMA_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

/// One persistence operation accepted from a project Rust callback.
///
/// Writes retain the exact document observed when the command is applied.
/// This prevents mutations from later callbacks from leaking into an earlier
/// save request while filesystem work waits for the service boundary.
#[derive(Debug, Clone, PartialEq)]
#[doc(hidden)]
pub enum GameSaveCommand {
    /// Persist an already-snapshotted document to one slot.
    Write {
        /// Destination slot number.
        slot: u32,
        /// Exact document snapshot captured during command application.
        data: SaveData,
    },
    /// Load one slot into the active save document.
    Load {
        /// Source slot number.
        slot: u32,
    },
}

/// Bounded bridge between exclusive project callbacks and save-slot IO.
#[derive(Debug, Default)]
#[doc(hidden)]
pub struct GameSaveCommandQueue {
    commands: VecDeque<GameSaveCommand>,
}

impl GameSaveCommandQueue {
    /// Returns the current number of preflighted persistence requests.
    #[doc(hidden)]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Returns whether there are no preflighted persistence requests.
    #[doc(hidden)]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Enqueues one request after the composition layer has checked capacity.
    #[doc(hidden)]
    pub fn push_preflighted(&mut self, command: GameSaveCommand) {
        assert!(
            self.commands.len() < MAX_GAME_SAVE_COMMANDS,
            "game save queue capacity must be checked during atomic preflight"
        );
        self.commands.push_back(command);
    }
}

/// Describes why a [`SaveData`] operation failed.
#[derive(Debug)]
pub enum SaveDataError {
    /// The JSON could not be parsed.
    Json(serde_json::Error),
    /// The document uses a schema version different from the current build.
    UnsupportedVersion {
        /// The version number found in the document.
        found: u32,
    },
}

impl fmt::Display for SaveDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "save data JSON error: {e}"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "save schema_version {found} is not supported (expected: {SAVE_SCHEMA_VERSION})"
            ),
        }
    }
}

impl std::error::Error for SaveDataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::UnsupportedVersion { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// SaveStore
// ---------------------------------------------------------------------------

/// Describes why a [`SaveStore`] operation failed.
#[derive(Debug)]
pub enum SaveStoreError {
    /// A filesystem operation failed for a reason other than a missing slot.
    Io(std::io::Error),
    /// The atomic replace-file step failed.
    Persist(PersistError),
    /// The slot file's contents were not a valid [`SaveData`] document.
    Data(SaveDataError),
    /// The requested slot has no file on disk.
    MissingSlot {
        /// The slot number that was requested.
        slot: u32,
    },
}

impl fmt::Display for SaveStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "save store I/O error: {e}"),
            Self::Persist(e) => write!(f, "save store persist error: {e}"),
            Self::Data(e) => write!(f, "save store data error: {e}"),
            Self::MissingSlot { slot } => write!(f, "save slot {slot} does not exist"),
        }
    }
}

impl std::error::Error for SaveStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Persist(e) => Some(e),
            Self::Data(e) => Some(e),
            Self::MissingSlot { .. } => None,
        }
    }
}

/// Owns a directory of `slot_<n>.save.json` files and performs atomic
/// slot IO (ADR 0048 §3).
///
/// Hosts choose the root directory: distributed players use the OS local-data
/// directory unless portable mode is explicit, while editor Play uses
/// `<project root>/saves/`. Writes
/// create the root directory on demand and replace slot files atomically
/// (temp file + rename, reusing the Phase 16-B manifest pattern via
/// [`engine_authoring::replace_file_contents`]), so a crash mid-write never
/// leaves a half-written slot file. The most recent operation's failure, if
/// any, is retained in [`SaveStore::last_error`] for script-visible
/// diagnostics (ADR 0048 §4); a successful operation clears it.
pub struct SaveStore {
    root: PathBuf,
    last_error: Option<String>,
}

/// Chooses the writable save root for a distributed desktop game.
///
/// Portable builds explicitly opt into a package-relative `saves` folder.
/// Normal builds use the OS local-data directory and fall back to the package
/// only when the platform cannot provide one.
#[cfg(not(target_arch = "wasm32"))]
pub fn distributed_save_root(project_name: &str, package_root: &Path, portable: bool) -> PathBuf {
    if portable {
        return package_root.join("saves");
    }
    let safe_name = project_name
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    dirs::data_local_dir()
        .map(|root| root.join("RustGameEngine").join(safe_name).join("saves"))
        .unwrap_or_else(|| package_root.join("saves"))
}

/// Chooses the writable log root for a distributed desktop game.
#[cfg(not(target_arch = "wasm32"))]
pub fn distributed_log_root(project_name: &str, package_root: &Path, portable: bool) -> PathBuf {
    if portable {
        return package_root.join("logs");
    }
    let safe_name = project_name
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    dirs::data_local_dir()
        .map(|root| root.join("RustGameEngine").join(safe_name).join("logs"))
        .unwrap_or_else(|| package_root.join("logs"))
}

/// Metadata used by save-slot selection and corrupt-slot recovery UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveSlotMetadata {
    /// Slot number parsed from the canonical file name.
    pub slot: u32,
    /// Whether the file exists but cannot be parsed as supported save data.
    pub corrupt: bool,
    /// Human-readable recovery diagnostic for corrupt or unreadable slots.
    pub diagnostic: Option<String>,
}

impl SaveStore {
    /// Creates a store rooted at `root`. The directory does not need to
    /// exist yet; [`SaveStore::write_slot`] creates it on demand.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            last_error: None,
        }
    }

    /// Returns the human-readable message from the most recent failed
    /// operation, or `None` if the last operation succeeded (or none has run
    /// yet).
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn slot_path(&self, slot: u32) -> PathBuf {
        self.root.join(format!("slot_{slot}.save.json"))
    }

    fn record<T>(&mut self, result: Result<T, SaveStoreError>) -> Result<T, SaveStoreError> {
        match &result {
            Ok(_) => self.last_error = None,
            Err(error) => self.last_error = Some(error.to_string()),
        }
        result
    }
}

/// Applies bounded project-Rust slot operations after gameplay callbacks.
///
/// The queue is always drained. Missing host resources and filesystem errors
/// are reported without panicking; a failed load leaves the active document
/// unchanged. This mirrors the existing script save contract while keeping IO
/// outside the exclusive GameModule callback.
#[doc(hidden)]
pub fn game_save_effect_system(
    mut queue: ResMut<GameSaveCommandQueue>,
    mut save_data: Option<ResMut<SaveData>>,
    mut save_store: Option<ResMut<SaveStore>>,
) {
    for command in queue.commands.drain(..) {
        match command {
            GameSaveCommand::Write { slot, data } => match save_store.as_deref_mut() {
                Some(store) => {
                    if let Err(error) = store.write_slot(slot, &data) {
                        log::error!("project Rust save_write(slot={slot}) failed: {error}");
                    }
                }
                None => log::error!(
                    "project Rust save_write(slot={slot}) queued without a SaveStore resource"
                ),
            },
            GameSaveCommand::Load { slot } => {
                match (save_store.as_deref_mut(), save_data.as_deref_mut()) {
                    (Some(store), Some(data)) => match store.read_slot(slot) {
                        Ok(loaded) => *data = loaded,
                        Err(error) => {
                            log::error!("project Rust save_load(slot={slot}) failed: {error}");
                        }
                    },
                    (None, _) => log::error!(
                        "project Rust save_load(slot={slot}) queued without a SaveStore resource"
                    ),
                    (_, None) => log::error!(
                        "project Rust save_load(slot={slot}) queued without a SaveData resource"
                    ),
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SaveStore {
    /// Writes `data` to `slot`, creating the root directory if needed.
    ///
    /// The write is atomic: `data` is serialized to a temporary sibling file
    /// which is then renamed over the target, so a reader never observes a
    /// partially written slot file.
    ///
    /// # Errors
    ///
    /// - [`SaveStoreError::Io`] if the root directory cannot be created.
    /// - [`SaveStoreError::Data`] if `data` cannot be serialized.
    /// - [`SaveStoreError::Persist`] if the atomic replace fails.
    pub fn write_slot(&mut self, slot: u32, data: &SaveData) -> Result<(), SaveStoreError> {
        let result = self.write_slot_impl(slot, data);
        self.record(result)
    }

    fn write_slot_impl(&self, slot: u32, data: &SaveData) -> Result<(), SaveStoreError> {
        std::fs::create_dir_all(&self.root).map_err(SaveStoreError::Io)?;
        let json = data
            .to_json_string()
            .map_err(|source| SaveStoreError::Data(SaveDataError::Json(source)))?;
        engine_authoring::replace_file_contents(&self.slot_path(slot), &json)
            .map_err(SaveStoreError::Persist)
    }

    /// Reads and parses the document stored at `slot`.
    ///
    /// # Errors
    ///
    /// - [`SaveStoreError::MissingSlot`] if no file exists for `slot`.
    /// - [`SaveStoreError::Io`] for other filesystem failures.
    /// - [`SaveStoreError::Data`] if the file is not a valid [`SaveData`]
    ///   document.
    pub fn read_slot(&mut self, slot: u32) -> Result<SaveData, SaveStoreError> {
        let result = self.read_slot_impl(slot);
        self.record(result)
    }

    fn read_slot_impl(&self, slot: u32) -> Result<SaveData, SaveStoreError> {
        let path = self.slot_path(slot);
        let json = std::fs::read_to_string(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                SaveStoreError::MissingSlot { slot }
            } else {
                SaveStoreError::Io(source)
            }
        })?;
        SaveData::from_json_str(&json).map_err(SaveStoreError::Data)
    }

    /// Returns every slot number with an existing file, in ascending order.
    ///
    /// Files that do not match the `slot_<n>.save.json` naming pattern are
    /// silently ignored. A missing or unreadable root directory yields an
    /// empty list rather than an error.
    pub fn list_slots(&self) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut slots: Vec<u32> = entries
            .flatten()
            .filter_map(|entry| parse_slot_file_name(entry.file_name().to_str()?))
            .collect();
        slots.sort_unstable();
        slots
    }

    /// Inspects every canonical slot without failing the complete slot list.
    pub fn slot_metadata(&self) -> Vec<SaveSlotMetadata> {
        self.list_slots()
            .into_iter()
            .map(|slot| match self.read_slot_impl(slot) {
                Ok(_) => SaveSlotMetadata {
                    slot,
                    corrupt: false,
                    diagnostic: None,
                },
                Err(error) => SaveSlotMetadata {
                    slot,
                    corrupt: true,
                    diagnostic: Some(error.to_string()),
                },
            })
            .collect()
    }

    /// Deletes the file for `slot`, if any.
    ///
    /// Deleting a slot that does not exist is not an error.
    ///
    /// # Errors
    ///
    /// Returns [`SaveStoreError::Io`] if the file exists but cannot be
    /// removed.
    pub fn delete_slot(&mut self, slot: u32) -> Result<(), SaveStoreError> {
        let result = self.delete_slot_impl(slot);
        self.record(result)
    }

    fn delete_slot_impl(&self, slot: u32) -> Result<(), SaveStoreError> {
        match std::fs::remove_file(self.slot_path(slot)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SaveStoreError::Io(error)),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_slot_file_name(name: &str) -> Option<u32> {
    name.strip_prefix("slot_")?
        .strip_suffix(".save.json")?
        .parse::<u32>()
        .ok()
}

/// wasm32 has no filesystem access (desktop-only IO, mirroring
/// the scene-loader filesystem stub): every operation fails with
/// [`SaveStoreError::Io`] and [`SaveStore::list_slots`] returns an empty list.
#[cfg(target_arch = "wasm32")]
impl SaveStore {
    /// Always fails on wasm32; see the module-level stub note.
    ///
    /// # Errors
    ///
    /// Always returns [`SaveStoreError::Io`].
    pub fn write_slot(&mut self, _slot: u32, _data: &SaveData) -> Result<(), SaveStoreError> {
        self.record(Err(unsupported_error()))
    }

    /// Always fails on wasm32; see the module-level stub note.
    ///
    /// # Errors
    ///
    /// Always returns [`SaveStoreError::Io`].
    pub fn read_slot(&mut self, _slot: u32) -> Result<SaveData, SaveStoreError> {
        self.record(Err(unsupported_error()))
    }

    /// Always returns an empty list on wasm32; see the module-level stub note.
    pub fn list_slots(&self) -> Vec<u32> {
        Vec::new()
    }

    /// Always returns an empty list on wasm32; see the module-level stub note.
    pub fn slot_metadata(&self) -> Vec<SaveSlotMetadata> {
        Vec::new()
    }

    /// Always fails on wasm32; see the module-level stub note.
    ///
    /// # Errors
    ///
    /// Always returns [`SaveStoreError::Io`].
    pub fn delete_slot(&mut self, _slot: u32) -> Result<(), SaveStoreError> {
        self.record(Err(unsupported_error()))
    }
}

#[cfg(target_arch = "wasm32")]
fn unsupported_error() -> SaveStoreError {
    SaveStoreError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "save IO is not available on wasm32",
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_value_round_trips_text_number_and_flag_as_untagged_json() {
        let text = SaveValue::Text("hello".to_string());
        let json = serde_json::to_string(&text).expect("must serialize");
        assert_eq!(json, "\"hello\"");
        assert_eq!(
            serde_json::from_str::<SaveValue>(&json).expect("must parse"),
            text
        );

        let number = SaveValue::Number(3.5);
        let json = serde_json::to_string(&number).expect("must serialize");
        assert_eq!(json, "3.5");
        assert_eq!(
            serde_json::from_str::<SaveValue>(&json).expect("must parse"),
            number
        );

        let flag = SaveValue::Flag(true);
        let json = serde_json::to_string(&flag).expect("must serialize");
        assert_eq!(json, "true");
        assert_eq!(
            serde_json::from_str::<SaveValue>(&json).expect("must parse"),
            flag
        );
    }

    #[test]
    fn save_data_missing_schema_version_is_rejected() {
        assert!(matches!(
            SaveData::from_json_str(r#"{"entries":{}}"#),
            Err(SaveDataError::Json(_))
        ));
    }

    #[test]
    fn save_data_missing_entries_is_rejected() {
        assert!(matches!(
            SaveData::from_json_str(r#"{"schema_version":1}"#),
            Err(SaveDataError::Json(_))
        ));
    }

    #[test]
    fn save_data_rejects_non_current_version() {
        let json = r#"{"schema_version":0,"entries":{}}"#;
        assert!(matches!(
            SaveData::from_json_str(json),
            Err(SaveDataError::UnsupportedVersion { found: 0 })
        ));
    }

    #[test]
    fn save_data_rejects_malformed_json() {
        assert!(matches!(
            SaveData::from_json_str("not json"),
            Err(SaveDataError::Json(_))
        ));
    }

    #[test]
    fn save_data_get_set_remove_keys_and_clear_round_trip() {
        let mut data = SaveData::new();
        assert_eq!(data.schema_version, SAVE_SCHEMA_VERSION);
        assert!(data.get("score").is_none());

        data.set("score", SaveValue::Number(42.0));
        data.set("player_name", SaveValue::Text("Rin".to_string()));
        assert_eq!(data.get("score"), Some(&SaveValue::Number(42.0)));
        assert_eq!(
            data.keys().collect::<Vec<_>>(),
            vec!["player_name", "score"]
        );

        assert_eq!(data.remove("score"), Some(SaveValue::Number(42.0)));
        assert!(data.get("score").is_none());

        data.clear();
        assert_eq!(data.keys().count(), 0);
    }

    #[test]
    fn save_data_json_round_trip_preserves_entries() {
        let mut data = SaveData::new();
        data.set("gold", SaveValue::Number(100.0));
        data.set("hardcore", SaveValue::Flag(false));

        let json = data.to_json_string().expect("must serialize");
        let parsed = SaveData::from_json_str(&json).expect("must parse");
        assert_eq!(parsed, data);
    }

    #[test]
    fn save_store_write_then_read_round_trips_data() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let mut store = SaveStore::new(dir.path().to_path_buf());

        let mut data = SaveData::new();
        data.set("chapter", SaveValue::Number(3.0));
        store.write_slot(0, &data).expect("write must succeed");

        let loaded = store.read_slot(0).expect("read must succeed");
        assert_eq!(loaded, data);
        assert!(store.last_error().is_none());
    }

    #[test]
    fn project_save_queue_writes_its_captured_snapshot_then_loads_it() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let mut captured = SaveData::new();
        captured.set("chapter", SaveValue::Number(2.0));
        let mut active = SaveData::new();
        active.set("chapter", SaveValue::Number(9.0));

        let mut queue = GameSaveCommandQueue::default();
        queue.push_preflighted(GameSaveCommand::Write {
            slot: 3,
            data: captured.clone(),
        });
        queue.push_preflighted(GameSaveCommand::Load { slot: 3 });

        let mut app = engine_ecs::App::new();
        app.insert_resource(queue);
        app.insert_resource(active);
        app.insert_resource(SaveStore::new(dir.path().to_path_buf()));
        app.add_system(game_save_effect_system);
        app.update().expect("save effect system must run");

        assert_eq!(app.world().get_resource::<SaveData>(), Some(&captured));
        assert_eq!(
            app.world()
                .get_resource::<GameSaveCommandQueue>()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn save_store_list_slots_is_ascending_and_ignores_foreign_files() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let mut store = SaveStore::new(dir.path().to_path_buf());
        store
            .write_slot(2, &SaveData::new())
            .expect("write must succeed");
        store
            .write_slot(0, &SaveData::new())
            .expect("write must succeed");
        store
            .write_slot(10, &SaveData::new())
            .expect("write must succeed");
        std::fs::write(dir.path().join("notes.txt"), "not a save file")
            .expect("must write foreign file");
        std::fs::write(dir.path().join("slot_x.save.json"), "{}")
            .expect("must write malformed slot file");

        assert_eq!(store.list_slots(), vec![0, 2, 10]);
    }

    #[test]
    fn slot_metadata_reports_corrupt_slots_without_hiding_them() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let mut store = SaveStore::new(dir.path().to_path_buf());
        store
            .write_slot(1, &SaveData::default())
            .expect("valid slot must write");
        std::fs::write(dir.path().join("slot_2.save.json"), "not json")
            .expect("corrupt slot fixture must write");

        let metadata = store.slot_metadata();

        assert_eq!(metadata.len(), 2);
        assert_eq!(metadata[0].slot, 1);
        assert!(!metadata[0].corrupt);
        assert_eq!(metadata[1].slot, 2);
        assert!(metadata[1].corrupt);
        assert!(metadata[1].diagnostic.is_some());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn portable_distributed_save_root_stays_beside_package() {
        let package = Path::new("C:/Games/My Game");
        assert_eq!(
            distributed_save_root("日本語 Project", package, true),
            package.join("saves")
        );
    }

    #[test]
    fn save_store_delete_slot_removes_the_file() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let mut store = SaveStore::new(dir.path().to_path_buf());
        store
            .write_slot(1, &SaveData::new())
            .expect("write must succeed");
        assert_eq!(store.list_slots(), vec![1]);

        store.delete_slot(1).expect("delete must succeed");
        assert!(store.list_slots().is_empty());

        // Deleting an already-absent slot is not an error.
        store
            .delete_slot(1)
            .expect("delete of missing slot must succeed");
    }

    #[test]
    fn save_store_read_missing_slot_returns_missing_slot_error() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let mut store = SaveStore::new(dir.path().to_path_buf());

        let result = store.read_slot(5);
        assert!(matches!(
            result,
            Err(SaveStoreError::MissingSlot { slot: 5 })
        ));
        assert!(store.last_error().is_some());
    }

    #[test]
    fn save_store_read_invalid_json_returns_data_error() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        std::fs::write(dir.path().join("slot_0.save.json"), "not valid json")
            .expect("must write malformed slot file");
        let mut store = SaveStore::new(dir.path().to_path_buf());

        assert!(matches!(
            store.read_slot(0),
            Err(SaveStoreError::Data(SaveDataError::Json(_)))
        ));
    }

    #[test]
    fn save_store_write_slot_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let mut store = SaveStore::new(dir.path().to_path_buf());
        store
            .write_slot(0, &SaveData::new())
            .expect("write must succeed");

        let leftover_temp_files: Vec<_> = std::fs::read_dir(dir.path())
            .expect("must read temp dir")
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(
            leftover_temp_files.is_empty(),
            "no temp files must remain after a successful write: {leftover_temp_files:?}"
        );
    }

    #[test]
    fn save_store_write_slot_clears_last_error_after_a_prior_failure() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let mut store = SaveStore::new(dir.path().to_path_buf());

        let _ = store.read_slot(0);
        assert!(store.last_error().is_some());

        store
            .write_slot(0, &SaveData::new())
            .expect("write must succeed");
        assert!(store.last_error().is_none());
    }
}
