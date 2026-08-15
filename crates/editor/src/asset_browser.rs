//! Asset browser: scans the `assets/` subtree and classifies files.
//!
//! This module is intentionally free of GUI dependencies so that all logic
//! can be tested without egui or eframe.  Rendering is done by a thin
//! `show_asset_browser` function in `crates/editor/src/ui/mod.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Semantic category for a file found under `assets/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    /// `*.scene.json`
    Scene,
    /// `*.graph.json`
    Graph,
    /// `*.graph.view.json`
    GraphView,
    /// `*.animset.json` author-owned motion-slot bindings.
    AnimationSet,
    /// Virtual imported animation-clip sub-asset.
    ///
    /// The file scanner never creates this kind directly; the selected model
    /// or motion source exposes it as a draggable stable sub-asset row.
    AnimationClip,
    /// `*.vmd` MMD motion source (ADR 0097 §3).
    ///
    /// Its own kind rather than [`Self::Mesh`] because a `.vmd` carries no
    /// geometry at all, and rather than [`Self::AnimationClip`] because that
    /// kind is the virtual sub-asset row a source exposes, not a file.
    MotionSource,
    /// `*.png`, `*.jpg`, `*.jpeg`, `*.webp`, `*.bmp`
    Texture,
    /// `*.obj`, `*.gltf`, `*.glb`, `*.fbx` (direct FBX import, ADR 0081),
    /// `*.pmx` (direct PMX import, ADR 0097)
    Mesh,
    /// `*.wav`, `*.ogg`, `*.mp3`, `*.flac`
    Audio,
    /// `*.material.json` (Phase 35)
    Material,
    /// `*.prefab.json` (Phase 33)
    Prefab,
    /// `*.ui.json` declarative UI document.
    UiDocument,
    /// `*.navmesh.json` baked navigation artifact.
    NavMesh,
    /// `*.retarget.json` animation retarget map (ADR 0079).
    RetargetMap,
    /// `*.rhai` (Phase 42)
    Script,
    /// Rust source below `assets/scripts/rust/` declaring a `GameComponent`.
    RustComponent,
    /// Rust source below `assets/scripts/rust/` declaring a `GameResource`.
    RustResource,
    /// Rust source below `assets/scripts/rust/` declaring a game system.
    RustSystem,
    /// Rust source below `assets/scripts/rust/` without engine declarations.
    RustModule,
}

impl AssetKind {
    /// Short display label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Scene => "[scene]",
            Self::Graph => "[graph]",
            Self::GraphView => "[view]",
            Self::AnimationSet => "[animset]",
            Self::AnimationClip => "[clip]",
            Self::MotionSource => "[motion]",
            Self::Texture => "[tex]",
            Self::Mesh => "[mesh]",
            Self::Audio => "[audio]",
            Self::Material => "[mat]",
            Self::Prefab => "[prefab]",
            Self::UiDocument => "[ui]",
            Self::NavMesh => "[nav]",
            Self::RetargetMap => "[retarget]",
            Self::Script => "[script]",
            Self::RustComponent => "[component]",
            Self::RustResource => "[resource]",
            Self::RustSystem => "[system]",
            Self::RustModule => "[module]",
        }
    }
}

/// A single file entry discovered by [`AssetBrowser::refresh`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEntry {
    /// Path relative to the root passed to the refresh operation.
    pub relative_path: PathBuf,
    /// Semantic category.
    pub kind: AssetKind,
    /// Display name: the file name with all recognized suffixes stripped.
    pub display_name: String,
}

/// One physical folder below the asset root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetFolder {
    /// Empty for the root, otherwise a normalized asset-relative directory.
    pub relative_path: PathBuf,
    /// Nesting depth used by compact tree presentation.
    pub depth: usize,
}

/// Scans the `assets/` directory and holds the resulting file list.
///
/// The list is populated by [`AssetBrowser::refresh`]. The editor shell calls
/// it after internal mutations and when its project filesystem watcher reports
/// an external change, while tests and other adapters may call it directly.
pub struct AssetBrowser {
    entries: Vec<AssetEntry>,
    folders: Vec<AssetFolder>,
    selected: Option<usize>,
    selected_paths: BTreeSet<PathBuf>,
    selected_folder: PathBuf,
    selected_folder_tile: Option<PathBuf>,
    collapsed_folders: BTreeSet<PathBuf>,
    pending_reveal: Option<PathBuf>,
}

impl AssetBrowser {
    /// Creates an empty browser (no entries loaded yet).
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            folders: vec![AssetFolder {
                relative_path: PathBuf::new(),
                depth: 0,
            }],
            selected: None,
            selected_paths: BTreeSet::new(),
            selected_folder: PathBuf::new(),
            selected_folder_tile: None,
            collapsed_folders: BTreeSet::new(),
            pending_reveal: None,
        }
    }

    /// Returns all discovered entries in the order they were found.
    pub fn entries(&self) -> &[AssetEntry] {
        &self.entries
    }

    /// Returns the index of the currently selected entry, if any.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Sets the selected entry by index.  Out-of-range values are clamped to
    /// `None`.
    pub fn set_selected(&mut self, index: Option<usize>) {
        self.selected = index.filter(|&i| i < self.entries.len());
        self.selected_paths.clear();
        self.selected_folder_tile = None;
        if let Some(path) = self
            .selected
            .and_then(|index| self.entries.get(index))
            .map(|entry| entry.relative_path.clone())
        {
            self.selected_paths.insert(path);
        }
    }

    /// Returns the currently selected entry, if any.
    pub fn selected_entry(&self) -> Option<&AssetEntry> {
        self.selected.and_then(|i| self.entries.get(i))
    }

    /// Returns discovered physical folders, including the empty root path.
    pub fn folders(&self) -> &[AssetFolder] {
        &self.folders
    }

    /// Returns the working folder whose direct children are shown.
    pub fn selected_folder(&self) -> &Path {
        &self.selected_folder
    }

    /// Returns the child-folder tile selected in the content grid.
    pub fn selected_folder_tile(&self) -> Option<&Path> {
        self.selected_folder_tile.as_deref()
    }

    /// Selects one discovered folder tile without opening that folder.
    ///
    /// Folder and file selection are mutually exclusive so the visible
    /// selection outline always identifies the target of a context action.
    pub fn select_folder_tile(&mut self, folder: &Path) -> bool {
        if folder.as_os_str().is_empty()
            || !self
                .folders
                .iter()
                .any(|candidate| candidate.relative_path == folder)
        {
            return false;
        }
        self.selected_folder_tile = Some(folder.to_path_buf());
        self.selected = None;
        self.selected_paths.clear();
        true
    }

    /// Selects a discovered folder and clears file selection.
    ///
    /// Every ancestor of `folder` is expanded so the working folder always has
    /// a visible tree row; otherwise navigation performed from another surface
    /// would appear to do nothing.
    pub fn set_selected_folder(&mut self, folder: impl Into<PathBuf>) -> bool {
        let folder = folder.into();
        if !self
            .folders
            .iter()
            .any(|candidate| candidate.relative_path == folder)
        {
            return false;
        }
        self.expand_ancestors(&folder);
        self.pending_reveal = Some(folder.clone());
        self.selected_folder = folder;
        self.selected = None;
        self.selected_paths.clear();
        self.selected_folder_tile = None;
        true
    }

    /// Returns whether `folder`'s children are hidden in the folder tree.
    pub fn is_folder_collapsed(&self, folder: &Path) -> bool {
        self.collapsed_folders.contains(folder)
    }

    /// Toggles whether `folder`'s children are hidden in the folder tree.
    pub fn toggle_folder_collapsed(&mut self, folder: &Path) {
        if !self.collapsed_folders.remove(folder) {
            self.collapsed_folders.insert(folder.to_path_buf());
        }
    }

    /// Returns whether `folder`'s tree row is visible.
    ///
    /// A collapsed folder hides its descendants but keeps its own row so it
    /// can be expanded again.
    pub fn folder_row_is_visible(&self, folder: &Path) -> bool {
        let mut ancestor = folder.parent();
        while let Some(path) = ancestor {
            if self.collapsed_folders.contains(path) {
                return false;
            }
            ancestor = path.parent();
        }
        true
    }

    /// Returns the folder whose tree row should be scrolled into view, if a
    /// navigation requested one since the last call.
    pub fn take_pending_reveal(&mut self) -> Option<PathBuf> {
        self.pending_reveal.take()
    }

    /// Expands every strict ancestor of `folder`.
    ///
    /// `folder` itself keeps its own collapsed state: opening a folder is
    /// about reaching its row, not about forcing its children open.
    fn expand_ancestors(&mut self, folder: &Path) {
        let mut ancestor = folder.parent();
        while let Some(path) = ancestor {
            self.collapsed_folders.remove(path);
            ancestor = path.parent();
        }
    }

    /// Returns indices for files directly inside the working folder.
    pub fn visible_entry_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.relative_path.parent().unwrap_or(Path::new(""))
                    == self.selected_folder.as_path()
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Returns all selected asset-relative paths in deterministic order.
    pub fn selected_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.selected_paths.iter()
    }

    /// Replaces or toggles one file in the multi-selection.
    pub fn select_path(&mut self, path: &Path, additive: bool) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.relative_path == path)
        else {
            return false;
        };
        if !additive {
            self.selected_paths.clear();
        }
        self.selected_folder_tile = None;
        if additive && !self.selected_paths.insert(path.to_path_buf()) {
            self.selected_paths.remove(path);
            if self.selected == Some(index) {
                self.selected = self.selected_paths.iter().next_back().and_then(|selected| {
                    self.entries
                        .iter()
                        .position(|entry| &entry.relative_path == selected)
                });
            }
        } else {
            self.selected_paths.insert(path.to_path_buf());
            self.selected = Some(index);
        }
        true
    }

    /// Scans `assets_root` up to `MAX_DEPTH` directory levels deep and
    /// replaces `entries` with the result.
    ///
    /// **Rules applied during the scan**:
    /// - Hidden entries (names starting with `.`) are skipped.
    /// - `*.graph.view.json` presentation sidecars are hidden because authors
    ///   interact with them through their semantic `*.graph.json` document.
    /// - Directories are traversed recursively up to `MAX_DEPTH` levels.
    /// - Symlinks are followed but the depth limit prevents infinite loops.
    /// - `asset_manifest.json` lives at the *project root* (one level above
    ///   `assets_root`), so it is naturally excluded.
    ///
    /// Selection follows its relative path across rescans even when indices
    /// shift, and is cleared only when the selected file disappeared.
    pub fn refresh(&mut self, assets_root: &Path) {
        let selected_path = self
            .selected_entry()
            .map(|entry| entry.relative_path.clone());
        let selected_paths = self.selected_paths.clone();
        let selected_folder = self.selected_folder.clone();
        let selected_folder_tile = self.selected_folder_tile.clone();
        self.entries.clear();
        self.folders.clear();
        self.folders.push(AssetFolder {
            relative_path: PathBuf::new(),
            depth: 0,
        });
        self.selected = None;
        self.selected_paths.clear();
        self.selected_folder_tile = None;

        if !assets_root.is_dir() {
            return;
        }

        let mut scratch = Vec::new();
        scan_dir(
            assets_root,
            assets_root,
            0,
            &mut scratch,
            &mut self.folders,
            classify_asset_path,
        );
        scratch.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        self.folders
            .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        self.entries = scratch;
        self.selected_folder = if self
            .folders
            .iter()
            .any(|folder| folder.relative_path == selected_folder)
        {
            selected_folder
        } else {
            PathBuf::new()
        };
        self.selected_paths
            .extend(selected_paths.into_iter().filter(|path| {
                self.entries
                    .iter()
                    .any(|entry| &entry.relative_path == path)
            }));
        self.selected_folder_tile = selected_folder_tile.filter(|path| {
            self.folders
                .iter()
                .any(|folder| &folder.relative_path == path)
        });
        // A folder that disappeared must not keep hiding a path that is later
        // recreated at the same location.
        self.collapsed_folders.retain(|path| {
            self.folders
                .iter()
                .any(|folder| &folder.relative_path == path)
        });
        if let Some(path) = selected_path {
            self.selected = self
                .entries
                .iter()
                .position(|entry| entry.relative_path == path);
        }
    }

    /// Selects the entry at `relative_path` after a refresh.
    ///
    /// Returns `true` when the path is present. This is used to keep a newly
    /// created script visible instead of clearing the user's context when the
    /// project browser rescans the filesystem.
    pub fn select_relative_path(&mut self, relative_path: &Path) -> bool {
        self.selected = self
            .entries
            .iter()
            .position(|entry| entry.relative_path == relative_path);
        self.selected_paths.clear();
        self.selected_folder_tile = None;
        if self.selected.is_some() {
            self.selected_paths.insert(relative_path.to_path_buf());
            let folder = relative_path
                .parent()
                .unwrap_or(Path::new(""))
                .to_path_buf();
            self.expand_ancestors(&folder);
            self.pending_reveal = Some(folder.clone());
            self.selected_folder = folder;
        }
        self.selected.is_some()
    }
}

/// One clickable step of the working-folder path, from the asset root to the
/// folder itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderBreadcrumb {
    /// Final path component, or `Assets` for the root.
    pub label: String,
    /// Asset-relative folder this step navigates to.
    pub folder: PathBuf,
}

/// Splits a working folder into the ordered path steps shown above the
/// content grid.
///
/// The first step is always the asset root, so a deeply nested folder can be
/// left one level at a time.
pub fn folder_breadcrumbs(folder: &Path) -> Vec<FolderBreadcrumb> {
    let mut breadcrumbs = vec![FolderBreadcrumb {
        label: "Assets".to_owned(),
        folder: PathBuf::new(),
    }];
    let mut current = PathBuf::new();
    for component in folder.components() {
        current.push(component);
        breadcrumbs.push(FolderBreadcrumb {
            label: component.as_os_str().to_string_lossy().into_owned(),
            folder: current.clone(),
        });
    }
    breadcrumbs
}

impl Default for AssetBrowser {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Scan logic (pure functions — testable without egui)
// ---------------------------------------------------------------------------

/// Defensive recursion cap; real projects nest asset folders freely, so this
/// only stops pathological trees and symlink cycles.
const MAX_DEPTH: u32 = 16;

fn scan_dir(
    root: &Path,
    dir: &Path,
    depth: u32,
    out: &mut Vec<AssetEntry>,
    folders: &mut Vec<AssetFolder>,
    classify: fn(&Path, &Path, &str) -> Option<(AssetKind, String)>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    for entry_result in read_dir {
        let Ok(entry) = entry_result else { continue };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with('.') {
            continue;
        }

        let full_path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        // Asset organization never follows symlinks because a visible folder
        // must not escape the project-owned asset root.
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if let Ok(relative_path) = full_path.strip_prefix(root) {
                folders.push(AssetFolder {
                    relative_path: relative_path.to_path_buf(),
                    depth: (depth + 1) as usize,
                });
            }
            scan_dir(root, &full_path, depth + 1, out, folders, classify);
        } else if file_type.is_file() {
            let Ok(relative_path) = full_path.strip_prefix(root) else {
                continue;
            };
            let Some((kind, display_name)) = classify(relative_path, &full_path, &name_str) else {
                continue;
            };
            out.push(AssetEntry {
                relative_path: relative_path.to_path_buf(),
                kind,
                display_name,
            });
        }
    }
}

fn classify_asset_path(
    relative_path: &Path,
    full_path: &Path,
    name: &str,
) -> Option<(AssetKind, String)> {
    let components = relative_path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => {
                Some(value.to_string_lossy().to_ascii_lowercase())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if components
        .first()
        .is_some_and(|component| component == "scripts")
    {
        if components
            .get(1)
            .is_some_and(|component| component == "rhai")
        {
            return classify_file_name(name).filter(|(kind, _)| *kind == AssetKind::Script);
        }
        if components
            .get(1)
            .is_some_and(|component| component == "rust")
        {
            let is_rust = relative_path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"));
            if !is_rust {
                return None;
            }
            return Some((
                classify_rust_source(full_path),
                relative_path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| relative_path.display().to_string()),
            ));
        }
        return None;
    }
    classify_file_name(name).filter(|(kind, _)| {
        !matches!(kind, AssetKind::Script | AssetKind::GraphView)
    })
}

/// Classifies one project Rust source by the declarations it contains.
///
/// The folder never decides the kind: a component keeps its badge after being
/// moved to any folder below `assets/scripts/rust/`, and a source without an
/// engine declaration is an ordinary compiled module. An unreadable file is
/// shown as an ordinary module so the browser still lists it.
fn classify_rust_source(full_path: &Path) -> AssetKind {
    let Ok(source) = std::fs::read_to_string(full_path) else {
        return AssetKind::RustModule;
    };
    match engine_authoring::rust_declarations(&source)
        .first()
        .map(|declaration| declaration.kind)
    {
        Some(engine_authoring::RustDeclarationKind::Component) => AssetKind::RustComponent,
        Some(engine_authoring::RustDeclarationKind::Resource) => AssetKind::RustResource,
        Some(engine_authoring::RustDeclarationKind::System) => AssetKind::RustSystem,
        None => AssetKind::RustModule,
    }
}

/// Classifies a supported file name and returns its kind and display name.
///
/// Multi-part extensions (`.scene.json`, `.graph.view.json`) are checked
/// before single extensions to give them priority.
/// Files without a recognized asset suffix return `None` and are excluded from
/// the Asset Browser.
pub fn classify_file_name(name: &str) -> Option<(AssetKind, String)> {
    let lower = name.to_ascii_lowercase();

    let (kind, suffix_len) = if lower.ends_with(".graph.view.json") {
        (AssetKind::GraphView, ".graph.view.json".len())
    } else if lower.ends_with(".scene.json") {
        (AssetKind::Scene, ".scene.json".len())
    } else if lower.ends_with(".animset.json") {
        (AssetKind::AnimationSet, ".animset.json".len())
    } else if lower.ends_with(".material.json") {
        (AssetKind::Material, ".material.json".len())
    } else if lower.ends_with(".prefab.json") {
        (AssetKind::Prefab, ".prefab.json".len())
    } else if lower.ends_with(".ui.json") {
        (AssetKind::UiDocument, ".ui.json".len())
    } else if lower.ends_with(".navmesh.json") {
        (AssetKind::NavMesh, ".navmesh.json".len())
    } else if lower.ends_with(".retarget.json") {
        (AssetKind::RetargetMap, ".retarget.json".len())
    } else if lower.ends_with(".graph.json") {
        (AssetKind::Graph, ".graph.json".len())
    } else if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
    {
        let ext_len = if lower.ends_with(".jpeg") || lower.ends_with(".webp") {
            5
        } else {
            4
        };
        (AssetKind::Texture, ext_len)
    } else if lower.ends_with(".obj")
        || lower.ends_with(".gltf")
        || lower.ends_with(".glb")
        || lower.ends_with(".fbx")
        || lower.ends_with(".pmx")
    {
        let ext_len = if lower.ends_with(".gltf") { 5 } else { 4 };
        (AssetKind::Mesh, ext_len)
    } else if lower.ends_with(".vmd") {
        (AssetKind::MotionSource, ".vmd".len())
    } else if lower.ends_with(".wav")
        || lower.ends_with(".ogg")
        || lower.ends_with(".mp3")
        || lower.ends_with(".flac")
    {
        let ext_len = if lower.ends_with(".flac") { 5 } else { 4 };
        (AssetKind::Audio, ext_len)
    } else if lower.ends_with(".rhai") {
        (AssetKind::Script, ".rhai".len())
    } else {
        return None;
    };

    let display_name = name[..name.len() - suffix_len].to_string();
    Some((kind, display_name))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // --- classify_file_name -------------------------------------------------

    fn classify_supported_file_name(name: &str) -> (AssetKind, String) {
        classify_file_name(name).expect("test file name must use a supported asset suffix")
    }

    #[test]
    fn classify_scene_json() {
        let (kind, name) = classify_supported_file_name("player.scene.json");
        assert_eq!(kind, AssetKind::Scene);
        assert_eq!(name, "player");
    }

    #[test]
    fn classify_graph_json() {
        let (kind, name) = classify_supported_file_name("ai.graph.json");
        assert_eq!(kind, AssetKind::Graph);
        assert_eq!(name, "ai");
    }

    #[test]
    fn classify_animation_set_json() {
        let (kind, name) = classify_file_name("hero.animset.json").expect("supported file");
        assert_eq!(kind, AssetKind::AnimationSet);
        assert_eq!(name, "hero");
    }

    #[test]
    fn classify_graph_view_json_takes_priority_over_graph_json() {
        let (kind, name) = classify_supported_file_name("ui.graph.view.json");
        assert_eq!(kind, AssetKind::GraphView);
        assert_eq!(name, "ui");
    }

    #[test]
    fn classify_texture_extensions() {
        for (file, expected_name) in [
            ("diffuse.png", "diffuse"),
            ("normal.jpg", "normal"),
            ("alpha.jpeg", "alpha"),
            ("envmap.webp", "envmap"),
            ("legacy.bmp", "legacy"),
        ] {
            let (kind, name) = classify_supported_file_name(file);
            assert_eq!(kind, AssetKind::Texture, "expected Texture for `{file}`");
            assert_eq!(name, expected_name);
        }
    }

    #[test]
    fn classify_mesh_extensions() {
        for (file, expected_name) in [
            ("cube.obj", "cube"),
            ("scene.gltf", "scene"),
            ("binary.glb", "binary"),
            ("legacy_character.fbx", "legacy_character"),
            ("mmd_character.pmx", "mmd_character"),
        ] {
            let (kind, name) = classify_supported_file_name(file);
            assert_eq!(kind, AssetKind::Mesh, "expected Mesh for `{file}`");
            assert_eq!(name, expected_name);
        }
    }

    #[test]
    fn classify_vmd_as_a_motion_source_not_a_mesh() {
        let (kind, name) = classify_supported_file_name("dance.vmd");
        // A `.vmd` carries no geometry, so classifying it as `Mesh` would put
        // it behind every mesh-gated browser affordance (instantiate, model
        // preview) that cannot apply to it.
        assert_eq!(kind, AssetKind::MotionSource);
        assert_eq!(name, "dance");
    }

    #[test]
    fn classify_audio_extensions() {
        for (file, expected_name) in [
            ("bgm.wav", "bgm"),
            ("jump.ogg", "jump"),
            ("theme.mp3", "theme"),
            ("ambient.flac", "ambient"),
        ] {
            let (kind, name) = classify_supported_file_name(file);
            assert_eq!(kind, AssetKind::Audio, "expected Audio for `{file}`");
            assert_eq!(name, expected_name);
        }
    }

    #[test]
    fn classify_material_json() {
        let (kind, name) = classify_supported_file_name("player.material.json");
        assert_eq!(kind, AssetKind::Material);
        assert_eq!(name, "player");
    }

    #[test]
    fn classify_prefab_json() {
        let (kind, name) = classify_supported_file_name("enemy.prefab.json");
        assert_eq!(kind, AssetKind::Prefab);
        assert_eq!(name, "enemy");
    }

    #[test]
    fn classify_ui_document_json() {
        let (kind, name) = classify_supported_file_name("hud.ui.json");
        assert_eq!(kind, AssetKind::UiDocument);
        assert_eq!(name, "hud");
    }

    #[test]
    fn classify_navmesh_json() {
        let (kind, name) = classify_supported_file_name("arena.navmesh.json");
        assert_eq!(kind, AssetKind::NavMesh);
        assert_eq!(name, "arena");
    }

    #[test]
    fn classify_retarget_map_json() {
        let (kind, name) = classify_supported_file_name("hero_to_mannequin.retarget.json");
        assert_eq!(kind, AssetKind::RetargetMap);
        assert_eq!(name, "hero_to_mannequin");
    }

    #[test]
    fn classify_rhai_script() {
        let (kind, name) = classify_supported_file_name("enemy_ai.rhai");
        assert_eq!(kind, AssetKind::Script);
        assert_eq!(name, "enemy_ai");
    }

    #[test]
    fn unsupported_file_name_is_not_classified() {
        assert_eq!(classify_file_name("readme.txt"), None);
    }

    #[test]
    fn classify_is_case_insensitive() {
        let (kind, _) = classify_supported_file_name("Hero.SCENE.JSON");
        assert_eq!(kind, AssetKind::Scene);
        let (kind, _) = classify_supported_file_name("Diffuse.PNG");
        assert_eq!(kind, AssetKind::Texture);
    }

    #[test]
    fn classify_dotfile_as_unsupported() {
        assert_eq!(classify_file_name(".gitkeep"), None);
    }

    // --- AssetBrowser::refresh ----------------------------------------------

    #[test]
    fn refresh_empty_assets_directory_produces_no_entries() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());
        assert!(browser.entries().is_empty());
    }

    #[test]
    fn refresh_nonexistent_directory_produces_no_entries() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let missing = dir.path().join("does_not_exist");
        let mut browser = AssetBrowser::new();
        browser.refresh(&missing);
        assert!(browser.entries().is_empty());
    }

    #[test]
    fn refresh_discovers_scene_and_graph_files() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let scenes = dir.path().join("scenes");
        let graphs = dir.path().join("graphs");
        fs::create_dir(&scenes).unwrap();
        fs::create_dir(&graphs).unwrap();
        fs::write(scenes.join("main.scene.json"), b"{}").unwrap();
        fs::write(graphs.join("player_ai.graph.json"), b"{}").unwrap();
        fs::write(graphs.join("player_ai.graph.view.json"), b"{}").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());

        assert_eq!(browser.entries().len(), 2);
        let kinds: Vec<_> = browser.entries().iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&AssetKind::Scene), "must find Scene");
        assert!(kinds.contains(&AssetKind::Graph), "must find Graph");
        assert!(
            !kinds.contains(&AssetKind::GraphView),
            "GraphView presentation sidecars must stay hidden behind their semantic Graph"
        );
    }

    #[test]
    fn refresh_lists_only_supported_asset_files() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        fs::write(dir.path().join("main.scene.json"), b"{}").unwrap();
        fs::write(dir.path().join("main.scene.json.autosave"), b"{}").unwrap();
        fs::write(dir.path().join("main.scene.json.bak"), b"{}").unwrap();
        fs::write(dir.path().join("notes.json"), b"{}").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());

        assert_eq!(browser.entries().len(), 1);
        assert_eq!(
            browser.entries()[0].relative_path,
            PathBuf::from("main.scene.json")
        );
        assert_eq!(browser.entries()[0].kind, AssetKind::Scene);
    }

    #[test]
    fn refresh_skips_hidden_files_and_directories() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        fs::write(dir.path().join(".hidden_file.scene.json"), b"{}").unwrap();
        let hidden_dir = dir.path().join(".hidden_dir");
        fs::create_dir(&hidden_dir).unwrap();
        fs::write(hidden_dir.join("inside.scene.json"), b"{}").unwrap();
        fs::write(dir.path().join("visible.scene.json"), b"{}").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());

        assert_eq!(
            browser.entries().len(),
            1,
            "only the visible file must be found"
        );
        assert_eq!(browser.entries()[0].display_name, "visible");
    }

    #[test]
    fn refresh_respects_depth_limit() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let mut path = dir.path().to_path_buf();
        for _ in 0..=MAX_DEPTH {
            path = path.join("sub");
            fs::create_dir(&path).unwrap();
        }
        fs::write(path.join("deep.scene.json"), b"{}").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());

        assert!(
            browser.entries().is_empty(),
            "file beyond MAX_DEPTH must not be listed; depth was {}",
            MAX_DEPTH + 1
        );
    }

    #[test]
    fn refresh_includes_file_at_max_depth() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let mut path = dir.path().to_path_buf();
        for _ in 0..MAX_DEPTH {
            path = path.join("sub");
            fs::create_dir(&path).unwrap();
        }
        fs::write(path.join("at_limit.scene.json"), b"{}").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());

        assert_eq!(
            browser.entries().len(),
            1,
            "file exactly at MAX_DEPTH must be listed"
        );
        assert_eq!(browser.entries()[0].kind, AssetKind::Scene);
    }

    #[test]
    fn refresh_preserves_selection_by_path_and_clears_missing_file() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        fs::write(dir.path().join("first.scene.json"), b"{}").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());
        browser.set_selected(Some(0));
        assert_eq!(browser.selected(), Some(0));

        browser.refresh(dir.path());
        assert_eq!(
            browser.selected(),
            Some(0),
            "selection must follow the same path across refresh"
        );

        fs::remove_file(dir.path().join("first.scene.json")).unwrap();
        browser.refresh(dir.path());
        assert_eq!(browser.selected(), None, "deleted selection must clear");
    }

    #[test]
    fn refresh_sorts_entries_by_relative_path() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        fs::write(dir.path().join("z_last.scene.json"), b"{}").unwrap();
        fs::write(dir.path().join("a_first.scene.json"), b"{}").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());

        assert_eq!(browser.entries().len(), 2);
        assert!(
            browser.entries()[0].relative_path < browser.entries()[1].relative_path,
            "entries must be sorted by relative_path"
        );
    }

    #[test]
    fn refresh_relative_paths_do_not_contain_assets_root_prefix() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let scenes = dir.path().join("scenes");
        fs::create_dir(&scenes).unwrap();
        fs::write(scenes.join("level1.scene.json"), b"{}").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());

        let rel = &browser.entries()[0].relative_path;
        assert!(
            !rel.is_absolute(),
            "relative_path must not be absolute: {rel:?}"
        );
        assert_eq!(rel, &PathBuf::from("scenes").join("level1.scene.json"));
    }

    // --- AssetBrowser selection helpers ------------------------------------

    #[test]
    fn set_selected_out_of_range_produces_none() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        fs::write(dir.path().join("x.scene.json"), b"{}").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());
        browser.set_selected(Some(99));
        assert_eq!(browser.selected(), None);
    }

    #[test]
    fn selected_entry_returns_entry_matching_index() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        fs::write(dir.path().join("a.scene.json"), b"{}").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());
        browser.set_selected(Some(0));

        let entry = browser.selected_entry().expect("entry must exist");
        assert_eq!(entry.kind, AssetKind::Scene);
    }

    #[test]
    fn folder_tile_selection_is_distinct_from_open_folder_and_clears_file_selection() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        fs::create_dir(dir.path().join("characters")).unwrap();
        fs::write(dir.path().join("main.scene.json"), b"{}").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());
        browser.set_selected(Some(0));

        assert!(browser.select_folder_tile(Path::new("characters")));
        assert_eq!(browser.selected_folder(), Path::new(""));
        assert_eq!(browser.selected_folder_tile(), Some(Path::new("characters")));
        assert_eq!(browser.selected(), None);
        assert_eq!(browser.selected_paths().count(), 0);
    }

    #[test]
    fn folder_tile_selection_survives_refresh_and_clears_when_folder_disappears() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        fs::create_dir(dir.path().join("characters")).unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());
        assert!(browser.select_folder_tile(Path::new("characters")));

        browser.refresh(dir.path());
        assert_eq!(browser.selected_folder_tile(), Some(Path::new("characters")));

        fs::remove_dir(dir.path().join("characters")).unwrap();
        browser.refresh(dir.path());
        assert_eq!(browser.selected_folder_tile(), None);
    }

    #[test]
    fn rust_script_kind_comes_from_the_source_not_the_folder() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        for folder in ["scripts/rhai", "scripts/rust/player", "scripts/rust/common"] {
            fs::create_dir_all(dir.path().join(folder)).unwrap();
        }
        fs::write(dir.path().join("scripts/rhai/ai.rhai"), "").unwrap();
        fs::write(
            dir.path().join("scripts/rust/player/health.rs"),
            "#[derive(engine::GameComponent)]\npub struct Health { pub enabled: bool }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("scripts/rust/player/state.rs"),
            "#[derive(Default, engine::GameResource)]\n#[game_resource(id = \"game.state\")]\npub struct State;\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("scripts/rust/player/tick.rs"),
            "#[engine::game_system(id = \"game.tick\", schedule = \"update\")]\nfn tick() {}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("scripts/rust/common/math.rs"),
            "pub fn calculate_damage() -> f32 { 10.0 }\n",
        )
        .unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());

        let kinds = browser
            .entries()
            .iter()
            .map(|entry| (entry.relative_path.clone(), entry.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                (PathBuf::from("scripts/rhai/ai.rhai"), AssetKind::Script),
                (
                    PathBuf::from("scripts/rust/common/math.rs"),
                    AssetKind::RustModule
                ),
                (
                    PathBuf::from("scripts/rust/player/health.rs"),
                    AssetKind::RustComponent
                ),
                (
                    PathBuf::from("scripts/rust/player/state.rs"),
                    AssetKind::RustResource
                ),
                (
                    PathBuf::from("scripts/rust/player/tick.rs"),
                    AssetKind::RustSystem
                ),
            ]
        );
    }

    #[test]
    fn select_relative_path_keeps_new_game_script_selected() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let components = dir.path().join("scripts/rust/components");
        fs::create_dir_all(&components).unwrap();
        fs::write(components.join("health.rs"), b"").unwrap();

        let mut browser = AssetBrowser::new();
        browser.refresh(dir.path());

        assert!(browser.select_relative_path(&PathBuf::from("scripts/rust/components/health.rs")));
        assert_eq!(browser.selected(), Some(0));
    }
}
