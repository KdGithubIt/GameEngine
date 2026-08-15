//! Safe project asset rename, move, and recoverable-delete operations.
//!
//! Files and manifest paths are updated as one editor operation. If manifest
//! persistence fails after a move, the file and in-memory manifest are rolled
//! back. Delete uses a project-local trash directory instead of unlinking the
//! source, so accidental removal remains recoverable.

use engine::{AssetManifest, ImportSettings, ManifestEntry};
use engine_authoring::{
    component_metadata_path, refresh_game_module_indexes, replace_file_contents, AssetId,
    PersistError, ProjectRoot,
};
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Result of moving one asset and updating any matching manifest entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetMoveReport {
    /// Previous asset-root-relative path.
    pub source: PathBuf,
    /// New asset-root-relative path, or trash path relative to the project.
    pub destination: PathBuf,
    /// Number of stable manifest entries whose path changed or was removed.
    pub manifest_entries: usize,
}

/// Result of one preflighted multi-file or folder relocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchAssetMoveReport {
    /// Individual source/destination pairs in application order.
    pub moves: Vec<AssetMoveReport>,
    /// Total manifest rows updated while preserving stable IDs.
    pub manifest_entries: usize,
    /// Whether the destination is recoverable project-local trash.
    pub is_trash: bool,
}

/// One external file copied into the project and committed to the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedExternalAsset {
    /// Original path supplied by the desktop file-drop event.
    pub source: PathBuf,
    /// Collision-free path relative to the project asset root.
    pub destination: PathBuf,
    /// Stable ID generated for the new manifest row.
    pub asset_id: AssetId,
}

/// One supported regular file discovered while preflighting an external drop.
///
/// `source` remains outside the project. `destination_parent` is relative to
/// the project asset root and preserves any directory hierarchy supplied by a
/// dropped folder. Keeping the validated UTF-8 filename separately prevents a
/// path that cannot be represented in the manifest from reaching the copy
/// phase.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalAssetImportCandidate {
    /// Absolute source file supplied directly or found below a dropped folder.
    source: PathBuf,
    /// Asset-root-relative directory that will contain the copied file.
    destination_parent: PathBuf,
    /// UTF-8 filename validated during the read-only preflight.
    file_name: String,
}

/// Per-file reason why an external drop item was not imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalAssetImportFailureKind {
    /// The filename does not match any runtime asset category.
    UnsupportedFormat,
    /// The operating-system copy operation failed.
    CopyFailed(String),
    /// The source did not expose a usable filename.
    InvalidFileName,
}

/// One rejected or failed file from a multi-file external drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalAssetImportFailure {
    /// Original path supplied by the desktop file-drop event.
    pub source: PathBuf,
    /// Actionable reason the file was not registered.
    pub kind: ExternalAssetImportFailureKind,
}

impl fmt::Display for ExternalAssetImportFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ExternalAssetImportFailureKind::UnsupportedFormat => write!(
                formatter,
                "unsupported asset format: {}",
                self.source.display()
            ),
            ExternalAssetImportFailureKind::CopyFailed(error) => write!(
                formatter,
                "failed to copy {}: {error}",
                self.source.display()
            ),
            ExternalAssetImportFailureKind::InvalidFileName => write!(
                formatter,
                "dropped path has no valid UTF-8 filename: {}",
                self.source.display()
            ),
        }
    }
}

/// Committed successes and independent per-file failures from one drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalAssetImportReport {
    /// Files whose copies and manifest rows were committed together.
    pub registered: Vec<ImportedExternalAsset>,
    /// Real filename or copy failures that rejected the complete batch.
    ///
    /// Unsupported formats are intentionally absent: external folder imports
    /// skip them instead of treating distribution-package extras as errors.
    pub failures: Vec<ExternalAssetImportFailure>,
}

/// Failure returned by safe asset management operations.
#[derive(Debug)]
pub enum AssetManagementError {
    /// A relative path was empty, absolute, or escaped with `..`.
    InvalidRelativePath(PathBuf),
    /// The source does not exist as a regular file.
    MissingSource(PathBuf),
    /// The destination already exists.
    DestinationExists(PathBuf),
    /// A folder was requested below itself.
    FolderCycle {
        /// Folder that would become its own ancestor.
        source: PathBuf,
        /// Rejected destination below `source`.
        destination: PathBuf,
    },
    /// A symlink was found in an operation whose complete target set must stay
    /// inside the physical asset root.
    SymlinkNotSupported(PathBuf),
    /// A batch selected both a folder and one of its descendants.
    OverlappingSources {
        /// Ancestor already selected by the same request.
        ancestor: PathBuf,
        /// Redundant descendant that would move twice.
        descendant: PathBuf,
    },
    /// A script source would leave its script root or enter a foreign one.
    ScriptMoveRestricted {
        /// Existing asset-relative source.
        source: PathBuf,
        /// Requested asset-relative destination.
        destination: PathBuf,
        /// Human-readable rule that rejected the operation.
        reason: &'static str,
    },
    /// A new file or folder cannot become a Rust module path.
    ScriptPlacementRestricted {
        /// Requested asset-relative destination.
        path: PathBuf,
        /// Human-readable rule that rejected the operation.
        reason: &'static str,
    },
    /// Generated Cargo bridge refresh failed after a script mutation.
    ScriptIndex(String),
    /// Rollback could not restore one or more exact paths.
    RollbackFailed {
        /// Exact paths still requiring manual recovery.
        paths: Vec<PathBuf>,
    },
    /// Filesystem mutation failed.
    Io(std::io::Error),
    /// Manifest serialization failed.
    ManifestJson(serde_json::Error),
    /// Atomic manifest persistence failed.
    Persist(PersistError),
}

impl fmt::Display for AssetManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRelativePath(path) => {
                write!(
                    formatter,
                    "invalid asset-relative path `{}`",
                    path.display()
                )
            }
            Self::MissingSource(path) => {
                write!(formatter, "asset does not exist: {}", path.display())
            }
            Self::DestinationExists(path) => {
                write!(
                    formatter,
                    "asset destination already exists: {}",
                    path.display()
                )
            }
            Self::FolderCycle {
                source,
                destination,
            } => write!(
                formatter,
                "cannot move asset folder `{}` into `{}` below itself",
                source.display(),
                destination.display()
            ),
            Self::SymlinkNotSupported(path) => write!(
                formatter,
                "asset operation rejected symlink `{}`",
                path.display()
            ),
            Self::OverlappingSources {
                ancestor,
                descendant,
            } => write!(
                formatter,
                "asset batch selects both `{}` and its descendant `{}`",
                ancestor.display(),
                descendant.display()
            ),
            Self::ScriptMoveRestricted {
                source,
                destination,
                reason,
            } => write!(
                formatter,
                "cannot move script `{}` to `{}`: {reason}",
                source.display(),
                destination.display()
            ),
            Self::ScriptIndex(error) => {
                write!(formatter, "could not refresh Rust script indexes: {error}")
            }
            Self::ScriptPlacementRestricted { path, reason } => {
                write!(formatter, "cannot create `{}`: {reason}", path.display())
            }
            Self::RollbackFailed { paths } => write!(
                formatter,
                "asset rollback failed for: {}",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Io(error) => write!(formatter, "asset filesystem operation failed: {error}"),
            Self::ManifestJson(error) => {
                write!(formatter, "manifest serialization failed: {error}")
            }
            Self::Persist(error) => write!(formatter, "manifest persistence failed: {error}"),
        }
    }
}

/// Copies desktop-dropped files or folders into one selected asset folder.
///
/// Folder inputs are traversed recursively, preserving the dropped folder's
/// name and its internal hierarchy. Only registerable regular files are copied
/// and committed to the manifest; unsupported files are silently ignored.
/// Existing files are never overwritten, and filename collisions receive the
/// same `_2`, `_3`, ... suffix convention used by editor-created assets.
///
/// A real filename or copy failure rejects the complete batch. If directory
/// creation, copying, or manifest persistence fails, every file and directory
/// created by this call is removed and `manifest` remains unchanged.
///
/// # Errors
///
/// Returns an [`AssetManagementError`] when the selected folder is invalid,
/// manifest persistence fails, or rollback cannot remove newly copied files.
pub fn import_external_asset_files(
    project: &ProjectRoot,
    manifest: &mut AssetManifest,
    sources: &[PathBuf],
    destination_folder: &Path,
) -> Result<ExternalAssetImportReport, AssetManagementError> {
    validate_relative_or_root(destination_folder)?;
    let destination_root = project.assets_root().join(destination_folder);
    if !destination_root.is_dir() {
        return Err(AssetManagementError::MissingSource(destination_root));
    }
    ensure_no_symlink_ancestors(&project.assets_root(), &destination_root)?;

    // Traverse every input before changing the project. Unsupported regular
    // files never enter the candidate list, while unsafe links and unreadable
    // directories stop the operation before it can become partial.
    let mut candidates = Vec::new();
    let mut failures = Vec::new();
    let mut sorted_sources = sources.to_vec();
    sorted_sources.sort();

    for source in &sorted_sources {
        collect_external_import_candidates(
            source,
            destination_folder,
            &mut candidates,
            &mut failures,
        )?;
    }

    // Selecting a directory and one of its descendants in the same desktop
    // drop must not register the same physical file twice.
    candidates.sort_by(|left, right| left.source.cmp(&right.source));
    candidates.dedup_by(|left, right| left.source == right.source);

    if !failures.is_empty() {
        return Ok(ExternalAssetImportReport {
            registered: Vec::new(),
            failures,
        });
    }

    // Track occupied names independently per destination directory. This
    // detects both existing project content and collisions within this batch
    // before any destination directory needs to exist.
    let mut occupied_by_parent = std::collections::BTreeMap::<
        PathBuf,
        std::collections::BTreeSet<String>,
    >::new();
    let mut planned_destinations = Vec::new();

    for candidate in candidates {
        if !occupied_by_parent.contains_key(&candidate.destination_parent) {
            let occupied = collect_occupied_import_names(
                project,
                manifest,
                &candidate.destination_parent,
            )?;
            occupied_by_parent.insert(candidate.destination_parent.clone(), occupied);
        }
        let occupied = occupied_by_parent
            .get_mut(&candidate.destination_parent)
            .expect("destination parent was inserted immediately above");
        let destination_name = collision_free_file_name(&candidate.file_name, occupied);
        let destination = candidate.destination_parent.join(destination_name);
        planned_destinations.push((candidate, destination));
    }

    // A folder containing only unsupported files is a successful no-op. Empty
    // project directories are not created for content the engine cannot use.
    if planned_destinations.is_empty() {
        return Ok(ExternalAssetImportReport {
            registered: Vec::new(),
            failures: Vec::new(),
        });
    }

    let destination_parents = planned_destinations
        .iter()
        .map(|(_, destination)| {
            destination
                .parent()
                .unwrap_or(Path::new(""))
                .to_path_buf()
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut created_directories = Vec::new();

    for parent in destination_parents {
        if let Err(error) =
            ensure_external_import_directory(project, &parent, &mut created_directories)
        {
            let rollback_failures =
                rollback_external_import(project, &[], &created_directories);
            if rollback_failures.is_empty() {
                return Err(error);
            }
            return Err(AssetManagementError::RollbackFailed {
                paths: rollback_failures,
            });
        }
    }

    // Stage manifest mutations separately so callers cannot observe IDs for
    // files whose batch later fails.
    let mut staged_manifest = manifest.clone();
    let mut registered = Vec::new();

    for (candidate, destination) in planned_destinations {
        let destination_absolute = project.assets_root().join(&destination);
        if let Err(error) = std::fs::copy(&candidate.source, &destination_absolute) {
            // Some platforms can leave a partial destination after a failed
            // copy. Remove it immediately; the main rollback handles every
            // previously completed copy.
            let _ = std::fs::remove_file(&destination_absolute);
            failures.push(ExternalAssetImportFailure {
                source: candidate.source,
                kind: ExternalAssetImportFailureKind::CopyFailed(error.to_string()),
            });
            continue;
        }

        let asset_id = AssetId::generate();
        let destination_name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .expect("candidate filenames are validated as UTF-8 during preflight");
        let display_name = asset_display_stem(destination_name);
        let name = unique_manifest_name(display_name, &staged_manifest);
        staged_manifest.insert(
            asset_id.clone(),
            ManifestEntry {
                path: normalize(&destination),
                name: Some(name),
                import_settings: ImportSettings::default(),
            },
        );
        registered.push(ImportedExternalAsset {
            source: candidate.source,
            destination,
            asset_id,
        });
    }

    if !failures.is_empty() {
        let rollback_failures =
            rollback_external_import(project, &registered, &created_directories);
        if !rollback_failures.is_empty() {
            return Err(AssetManagementError::RollbackFailed {
                paths: rollback_failures,
            });
        }
        return Ok(ExternalAssetImportReport {
            registered: Vec::new(),
            failures,
        });
    }

    if let Err(error) = persist_manifest(project, &staged_manifest) {
        let rollback_failures =
            rollback_external_import(project, &registered, &created_directories);
        if rollback_failures.is_empty() {
            return Err(error);
        }
        return Err(AssetManagementError::RollbackFailed {
            paths: rollback_failures,
        });
    }

    *manifest = staged_manifest;
    Ok(ExternalAssetImportReport {
        registered,
        failures,
    })
}

/// Recursively collects supported regular files from one external drop item.
///
/// A directory contributes its own name to `destination_parent`, preserving
/// relative references such as a PMX source's `textures/face.png`. Unsupported
/// regular files and non-file special entries are ignored. Symlinks are
/// rejected because following one could copy data outside the directory the
/// user selected.
fn collect_external_import_candidates(
    source: &Path,
    destination_parent: &Path,
    candidates: &mut Vec<ExternalAssetImportCandidate>,
    failures: &mut Vec<ExternalAssetImportFailure>,
) -> Result<(), AssetManagementError> {
    let Some(file_name) = source.file_name().and_then(|name| name.to_str()) else {
        failures.push(ExternalAssetImportFailure {
            source: source.to_path_buf(),
            kind: ExternalAssetImportFailureKind::InvalidFileName,
        });
        return Ok(());
    };

    let metadata = match std::fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(_) if is_registerable_asset_path(Path::new(file_name)) => {
            // Preserve the previous per-file CopyFailed report for a supported
            // path that disappears between the desktop event and this call.
            validate_import_script_placement(source, destination_parent, file_name)?;
            candidates.push(ExternalAssetImportCandidate {
                source: source.to_path_buf(),
                destination_parent: destination_parent.to_path_buf(),
                file_name: file_name.to_owned(),
            });
            return Ok(());
        }
        Err(error) => return Err(AssetManagementError::Io(error)),
    };

    if metadata.file_type().is_symlink() {
        return Err(AssetManagementError::SymlinkNotSupported(
            source.to_path_buf(),
        ));
    }

    if metadata.is_dir() {
        let child_destination_parent = destination_parent.join(file_name);
        let mut children = std::fs::read_dir(source)
            .map_err(AssetManagementError::Io)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(AssetManagementError::Io)?;
        children.sort_by_key(|entry| entry.file_name());

        for child in children {
            collect_external_import_candidates(
                &child.path(),
                &child_destination_parent,
                candidates,
                failures,
            )?;
        }
        return Ok(());
    }

    if !metadata.is_file() || !is_registerable_asset_path(Path::new(file_name)) {
        return Ok(());
    }

    validate_import_script_placement(source, destination_parent, file_name)?;
    candidates.push(ExternalAssetImportCandidate {
        source: source.to_path_buf(),
        destination_parent: destination_parent.to_path_buf(),
        file_name: file_name.to_owned(),
    });
    Ok(())
}

/// Collects case-insensitive names already occupied in one destination.
///
/// Both physical entries and manifest rows participate so a stale manifest or
/// an unregistered file cannot be overwritten by an external drop.
fn collect_occupied_import_names(
    project: &ProjectRoot,
    manifest: &AssetManifest,
    destination_parent: &Path,
) -> Result<std::collections::BTreeSet<String>, AssetManagementError> {
    let absolute = project.assets_root().join(destination_parent);
    let mut occupied = std::collections::BTreeSet::new();

    if absolute.exists() {
        if !absolute.is_dir() {
            return Err(AssetManagementError::DestinationExists(absolute));
        }
        ensure_no_symlink_ancestors(&project.assets_root(), &absolute)?;
        for entry in std::fs::read_dir(&absolute).map_err(AssetManagementError::Io)? {
            let entry = entry.map_err(AssetManagementError::Io)?;
            occupied.insert(entry.file_name().to_string_lossy().to_ascii_lowercase());
        }
    }

    occupied.extend(manifest.iter().filter_map(|(_, entry)| {
        let path = PathBuf::from(entry.path.replace('\\', "/"));
        if path.parent().unwrap_or(Path::new("")) != destination_parent {
            return None;
        }
        path.file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
    }));
    Ok(occupied)
}

fn validate_import_script_placement(
    source: &Path,
    destination_folder: &Path,
    file_name: &str,
) -> Result<(), AssetManagementError> {
    let destination = destination_folder.join(file_name);
    let is_rhai = Path::new(file_name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rhai"));
    let allowed = match script_root(&destination) {
        Some(ScriptRoot::Rhai) => is_rhai,
        // External drops are registered asset imports; Rust sources reach the
        // project through the editor's create and move commands instead.
        Some(ScriptRoot::Rust) => false,
        None => !destination.starts_with("scripts") && !is_rhai,
    };
    if allowed {
        Ok(())
    } else {
        Err(AssetManagementError::ScriptMoveRestricted {
            source: source.to_path_buf(),
            destination,
            reason: "the destination folder only accepts its declared script type",
        })
    }
}

/// Returns whether a path can be represented by the existing asset manifest
/// registration categories.
pub fn is_registerable_asset_path(path: &Path) -> bool {
    [
        engine::AssetKind::Mesh,
        engine::AssetKind::Material,
        engine::AssetKind::Texture,
        engine::AssetKind::AnimationClip,
        engine::AssetKind::AnimationGraph,
        engine::AssetKind::AnimationSet,
        engine::AssetKind::BehaviorTree,
        engine::AssetKind::Audio,
        engine::AssetKind::NavMesh,
        engine::AssetKind::UiDocument,
        engine::AssetKind::Prefab,
    ]
    .into_iter()
    .any(|kind| engine::asset_path_matches_kind(kind, path))
}

fn collision_free_file_name(
    requested: &str,
    occupied_names: &mut std::collections::BTreeSet<String>,
) -> String {
    let normalized = requested.to_ascii_lowercase();
    if occupied_names.insert(normalized) {
        return requested.to_owned();
    }

    let (stem, suffix) = split_asset_file_name(requested);
    for index in 2_u32.. {
        let candidate = format!("{stem}_{index}{suffix}");
        if occupied_names.insert(candidate.to_ascii_lowercase()) {
            return candidate;
        }
    }
    unreachable!("u32 filename suffix space must not be exhausted")
}

fn split_asset_file_name(file_name: &str) -> (&str, &str) {
    let lower = file_name.to_ascii_lowercase();
    for suffix in [
        ".material.json",
        ".prefab.json",
        ".navmesh.json",
        ".graph.json",
        ".ui.json",
    ] {
        if lower.ends_with(suffix) {
            let split = file_name.len() - suffix.len();
            return (&file_name[..split], &file_name[split..]);
        }
    }
    match file_name.rfind('.') {
        Some(index) if index > 0 => (&file_name[..index], &file_name[index..]),
        _ => (file_name, ""),
    }
}

fn asset_display_stem(file_name: &str) -> &str {
    split_asset_file_name(file_name).0
}

fn unique_manifest_name(display_name: &str, manifest: &AssetManifest) -> String {
    let base = asset_name_slug(display_name);
    if !manifest
        .iter()
        .any(|(_, entry)| entry.name.as_deref() == Some(base.as_str()))
    {
        return base;
    }
    for index in 2_u32.. {
        let candidate = format!("{base}_{index}");
        if !manifest
            .iter()
            .any(|(_, entry)| entry.name.as_deref() == Some(candidate.as_str()))
        {
            return candidate;
        }
    }
    unreachable!("u32 manifest name suffix space must not be exhausted")
}

fn asset_name_slug(display_name: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_separator = false;
    for character in display_name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator && !slug.is_empty() {
            slug.push('_');
            previous_was_separator = true;
        }
    }
    let slug = slug.trim_matches('_');
    if slug.is_empty() {
        "asset".to_owned()
    } else {
        slug.to_owned()
    }
}

/// Creates every missing directory required by one external import target.
///
/// Only directories created by this call are appended to
/// `created_directories`. Existing project directories are therefore never
/// eligible for rollback removal.
fn ensure_external_import_directory(
    project: &ProjectRoot,
    relative: &Path,
    created_directories: &mut Vec<PathBuf>,
) -> Result<(), AssetManagementError> {
    validate_relative_or_root(relative)?;
    if relative.as_os_str().is_empty() {
        return Ok(());
    }

    let mut cursor = relative.to_path_buf();
    let mut missing = Vec::new();
    while !cursor.as_os_str().is_empty() && !project.assets_root().join(&cursor).exists() {
        missing.push(cursor.clone());
        cursor.pop();
    }

    let existing_ancestor = project.assets_root().join(&cursor);
    if !existing_ancestor.is_dir() {
        return Err(AssetManagementError::DestinationExists(
            existing_ancestor,
        ));
    }
    ensure_no_symlink_ancestors(&project.assets_root(), &existing_ancestor)?;

    for directory in missing.into_iter().rev() {
        let absolute = project.assets_root().join(&directory);
        std::fs::create_dir(&absolute).map_err(AssetManagementError::Io)?;
        created_directories.push(directory);
    }
    Ok(())
}

/// Removes files and directories created by a failed external import.
///
/// Files are removed before directories, and directories are removed in
/// reverse creation order. Missing paths count as already rolled back; any
/// other removal failure is returned for explicit manual recovery.
fn rollback_external_import(
    project: &ProjectRoot,
    registered: &[ImportedExternalAsset],
    created_directories: &[PathBuf],
) -> Vec<PathBuf> {
    let mut failures = Vec::new();
    for imported in registered.iter().rev() {
        let path = project.assets_root().join(&imported.destination);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => failures.push(path),
        }
    }
    for directory in created_directories.iter().rev() {
        let path = project.assets_root().join(directory);
        match std::fs::remove_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => failures.push(path),
        }
    }
    failures
}

/// Creates one physical folder below `assets/` after validating its path.
pub fn create_asset_folder(
    project: &ProjectRoot,
    relative_path: &Path,
) -> Result<PathBuf, AssetManagementError> {
    validate_relative(relative_path)?;
    validate_script_folder_path(relative_path)?;
    let destination = project.assets_root().join(relative_path);
    if destination.exists() {
        return Err(AssetManagementError::DestinationExists(destination));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| AssetManagementError::InvalidRelativePath(relative_path.to_path_buf()))?;
    ensure_no_symlink_ancestors(&project.assets_root(), parent)?;
    std::fs::create_dir(&destination).map_err(AssetManagementError::Io)?;
    Ok(relative_path.to_path_buf())
}

/// Rejects folders below the Rust script root that cannot become module paths.
///
/// Folder organization below `scripts/rust/` is free-form, but each segment
/// becomes a Rust module name in the generated index, so the same name rules
/// apply to a folder the author creates by hand.
fn validate_script_folder_path(path: &Path) -> Result<(), AssetManagementError> {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if components.first().map(String::as_str) != Some("scripts")
        || components.get(1).map(String::as_str) != Some("rust")
    {
        return Ok(());
    }
    for module in &components[2..] {
        if !is_rust_module_name(module) {
            return Err(AssetManagementError::ScriptPlacementRestricted {
                path: path.to_path_buf(),
                reason: "Rust folders must be valid Rust module names",
            });
        }
    }
    Ok(())
}

/// Returns whether one path segment can be used as a Rust module name.
fn is_rust_module_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

/// Renames or moves one file or complete folder to an exact relative path.
pub fn move_asset_path(
    project: &ProjectRoot,
    manifest: &mut AssetManifest,
    source: &Path,
    destination: &Path,
) -> Result<BatchAssetMoveReport, AssetManagementError> {
    validate_relative(source)?;
    validate_relative(destination)?;
    let source_absolute = project.assets_root().join(source);
    let metadata = std::fs::symlink_metadata(&source_absolute)
        .map_err(|_| AssetManagementError::MissingSource(source_absolute.clone()))?;
    if metadata.file_type().is_symlink() {
        return Err(AssetManagementError::SymlinkNotSupported(source_absolute));
    }
    if metadata.is_dir() {
        ensure_tree_has_no_symlink(&source_absolute)?;
        if destination.starts_with(source) {
            return Err(AssetManagementError::FolderCycle {
                source: source.to_path_buf(),
                destination: destination.to_path_buf(),
            });
        }
    }
    validate_script_move_tree(project, source, destination, metadata.is_dir())?;
    let destination_absolute = project.assets_root().join(destination);
    if destination_absolute.exists() {
        return Err(AssetManagementError::DestinationExists(
            destination_absolute,
        ));
    }
    let parent = destination_absolute
        .parent()
        .ok_or_else(|| AssetManagementError::InvalidRelativePath(destination.to_path_buf()))?;
    ensure_no_symlink_ancestors(&project.assets_root(), parent)?;
    execute_move_plan(
        project,
        manifest,
        vec![(source.to_path_buf(), destination.to_path_buf())],
        false,
    )
}

/// Moves files or folders into one existing asset folder as one rollback-safe
/// operation while preserving every matching manifest ID.
pub fn move_asset_batch(
    project: &ProjectRoot,
    manifest: &mut AssetManifest,
    sources: &[PathBuf],
    destination_folder: &Path,
) -> Result<BatchAssetMoveReport, AssetManagementError> {
    validate_relative_or_root(destination_folder)?;
    let destination_root = project.assets_root().join(destination_folder);
    if !destination_root.is_dir() {
        return Err(AssetManagementError::MissingSource(destination_root));
    }
    ensure_no_symlink_ancestors(&project.assets_root(), &destination_root)?;
    let mut unique = std::collections::BTreeSet::new();
    let mut destinations = std::collections::BTreeSet::new();
    let mut plan = Vec::new();
    for source in sources {
        validate_relative(source)?;
        if !unique.insert(source.clone()) {
            continue;
        }
        let source_absolute = project.assets_root().join(source);
        let metadata = std::fs::symlink_metadata(&source_absolute)
            .map_err(|_| AssetManagementError::MissingSource(source_absolute.clone()))?;
        if metadata.file_type().is_symlink() {
            return Err(AssetManagementError::SymlinkNotSupported(source_absolute));
        }
        if metadata.is_dir() {
            ensure_tree_has_no_symlink(&source_absolute)?;
        }
        let name = source
            .file_name()
            .ok_or_else(|| AssetManagementError::InvalidRelativePath(source.clone()))?;
        let destination = destination_folder.join(name);
        let destination_absolute = project.assets_root().join(&destination);
        if destination_absolute.exists() || !destinations.insert(destination.clone()) {
            return Err(AssetManagementError::DestinationExists(
                destination_absolute,
            ));
        }
        if metadata.is_dir() && destination.starts_with(source) {
            return Err(AssetManagementError::FolderCycle {
                source: source.clone(),
                destination,
            });
        }
        validate_script_move_tree(project, source, &destination, metadata.is_dir())?;
        plan.push((source.clone(), destination));
    }
    reject_overlapping_sources(&unique)?;
    execute_move_plan(project, manifest, plan, false)
}

/// Moves files or complete folders into a unique project-local trash batch.
pub fn move_asset_paths_to_trash(
    project: &ProjectRoot,
    manifest: &mut AssetManifest,
    sources: &[PathBuf],
) -> Result<BatchAssetMoveReport, AssetManagementError> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let trash_relative = PathBuf::from(".engine")
        .join("asset_trash")
        .join(format!("batch_{stamp}"));
    let trash_absolute = project.path().join(&trash_relative);
    let mut plan = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for source in sources {
        validate_relative(source)?;
        let source_absolute = project.assets_root().join(source);
        let metadata = std::fs::symlink_metadata(&source_absolute)
            .map_err(|_| AssetManagementError::MissingSource(source_absolute.clone()))?;
        if metadata.file_type().is_symlink() {
            return Err(AssetManagementError::SymlinkNotSupported(source_absolute));
        }
        if metadata.is_dir() {
            ensure_tree_has_no_symlink(&source_absolute)?;
        }
        let name = source
            .file_name()
            .ok_or_else(|| AssetManagementError::InvalidRelativePath(source.clone()))?;
        if !names.insert(name.to_os_string()) {
            return Err(AssetManagementError::DestinationExists(
                trash_absolute.join(name),
            ));
        }
        plan.push((source.clone(), trash_relative.join(name)));
    }
    let selected = plan
        .iter()
        .map(|(source, _)| source.clone())
        .collect::<std::collections::BTreeSet<_>>();
    reject_overlapping_sources(&selected)?;
    execute_move_plan(project, manifest, plan, true)
}

fn reject_overlapping_sources(
    sources: &std::collections::BTreeSet<PathBuf>,
) -> Result<(), AssetManagementError> {
    for descendant in sources {
        if let Some(ancestor) = sources
            .iter()
            .find(|candidate| *candidate != descendant && descendant.starts_with(candidate))
        {
            return Err(AssetManagementError::OverlappingSources {
                ancestor: ancestor.clone(),
                descendant: descendant.clone(),
            });
        }
    }
    Ok(())
}

/// Script root that owns one asset-relative path.
///
/// Only the two script languages are distinguished. Folders below
/// `scripts/rust/` are free-form: a Rust source keeps whatever it declares when
/// it moves, so there is no per-kind category to enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptRoot {
    Rhai,
    Rust,
}

fn script_root(path: &Path) -> Option<ScriptRoot> {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect::<Vec<_>>();
    match components.as_slice() {
        [scripts, language, ..] if scripts == "scripts" => match language.as_str() {
            "rhai" => Some(ScriptRoot::Rhai),
            "rust" => Some(ScriptRoot::Rust),
            _ => None,
        },
        _ => None,
    }
}

fn validate_script_move_tree(
    project: &ProjectRoot,
    source: &Path,
    destination: &Path,
    is_directory: bool,
) -> Result<(), AssetManagementError> {
    validate_script_move_pair(source, destination, is_directory)?;
    if !is_directory {
        return Ok(());
    }
    let source_absolute = project.assets_root().join(source);
    let mut pending = vec![source_absolute];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(AssetManagementError::Io)? {
            let entry = entry.map_err(AssetManagementError::Io)?;
            let file_type = entry.file_type().map_err(AssetManagementError::Io)?;
            if file_type.is_symlink() {
                return Err(AssetManagementError::SymlinkNotSupported(entry.path()));
            }
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(project.assets_root())
                .expect("asset tree entry stays below assets")
                .to_path_buf();
            let suffix = relative
                .strip_prefix(source)
                .expect("descendant stays below selected source");
            validate_script_move_pair(&relative, &destination.join(suffix), false)?;
        }
    }
    Ok(())
}

/// Validates one relocation against the two script-root rules.
///
/// A source may move freely inside `scripts/rust/`, including between the
/// recommended `components`, `resources`, `systems`, and `shared` folders,
/// because the generated module index is rebuilt from the resulting tree and
/// the declared kind comes from the source text. Leaving the root, entering it
/// with a foreign file type, and folder names that cannot become Rust module
/// names remain rejected.
fn validate_script_move_pair(
    source: &Path,
    destination: &Path,
    is_directory: bool,
) -> Result<(), AssetManagementError> {
    let source_root = script_root(source);
    let destination_root = script_root(destination);
    let destination_is_noncanonical_script = destination_root.is_none()
        && destination.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("rhai") || extension.eq_ignore_ascii_case("rs")
        });
    if destination_is_noncanonical_script {
        return Err(AssetManagementError::ScriptMoveRestricted {
            source: source.to_path_buf(),
            destination: destination.to_path_buf(),
            reason: "script extensions are only valid inside their script root",
        });
    }
    if source_root != destination_root && (source_root.is_some() || destination_root.is_some()) {
        return Err(AssetManagementError::ScriptMoveRestricted {
            source: source.to_path_buf(),
            destination: destination.to_path_buf(),
            reason: "scripts must remain inside their original script root",
        });
    }
    if destination_root == Some(ScriptRoot::Rust) {
        validate_script_folder_path(destination.parent().unwrap_or(Path::new("")))?;
        if is_directory
            && !destination
                .file_name()
                .is_some_and(|name| is_rust_module_name(&name.to_string_lossy()))
        {
            return Err(AssetManagementError::ScriptPlacementRestricted {
                path: destination.to_path_buf(),
                reason: "Rust folders must be valid Rust module names",
            });
        }
    }
    if is_directory {
        return Ok(());
    }
    let valid_extension = match source_root {
        Some(ScriptRoot::Rhai) => [source, destination].iter().all(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rhai"))
        }),
        Some(ScriptRoot::Rust) => [source, destination].iter().all(is_rust_source_or_sidecar),
        None => true,
    };
    if !valid_extension {
        return Err(AssetManagementError::ScriptMoveRestricted {
            source: source.to_path_buf(),
            destination: destination.to_path_buf(),
            reason: "this folder only accepts its declared script type",
        });
    }
    if destination_root == Some(ScriptRoot::Rust)
        && !destination
            .file_name()
            .is_some_and(|name| is_rust_module_name(rust_source_stem(&name.to_string_lossy())))
    {
        return Err(AssetManagementError::ScriptPlacementRestricted {
            path: destination.to_path_buf(),
            reason: "Rust file names must be valid Rust module names",
        });
    }
    Ok(())
}

fn is_rust_source_or_sidecar(path: &&Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
        || path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".rs.meta.json"))
}

fn rust_source_stem(file_name: &str) -> &str {
    file_name
        .strip_suffix(".rs.meta.json")
        .or_else(|| file_name.strip_suffix(".rs"))
        .unwrap_or(file_name)
}

/// Adds the `.rs.meta.json` sidecar of every moved Rust source to the plan.
///
/// Component identity lives in the sidecar, so the pair must move as one
/// operation for the stable component ID and every scene, prefab, and Inspector
/// reference to survive. Sources without a sidecar are ordinary modules and
/// need no companion, and a sidecar already selected by the same request is not
/// planned twice.
fn expand_component_sidecars(
    project: &ProjectRoot,
    mut plan: Vec<(PathBuf, PathBuf)>,
) -> Result<Vec<(PathBuf, PathBuf)>, AssetManagementError> {
    let mut companions = Vec::new();
    for (source, destination) in &plan {
        if script_root(source) != Some(ScriptRoot::Rust)
            || !source
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
            || project.assets_root().join(source).is_dir()
        {
            continue;
        }
        let source_sidecar = component_metadata_path(source);
        if !project.assets_root().join(&source_sidecar).is_file()
            || plan.iter().any(|(planned, _)| planned == &source_sidecar)
        {
            continue;
        }
        let destination_sidecar = component_metadata_path(destination);
        let destination_absolute = project.assets_root().join(&destination_sidecar);
        if destination_absolute.exists() {
            return Err(AssetManagementError::DestinationExists(
                destination_absolute,
            ));
        }
        companions.push((source_sidecar, destination_sidecar));
    }
    plan.extend(companions);
    Ok(plan)
}

/// Adds the optional `*.graph.view.json` presentation sidecar of every moved
/// semantic graph to the same rollback-safe operation.
///
/// Graph views are intentionally hidden from the Asset Browser because they
/// are not independently authorable or manifest-registered assets. Moving or
/// deleting the visible semantic graph must therefore carry its hidden view;
/// otherwise an orphan would remain at the old path and the moved graph would
/// silently lose its saved layout.
fn expand_graph_view_sidecars(
    project: &ProjectRoot,
    mut plan: Vec<(PathBuf, PathBuf)>,
) -> Result<Vec<(PathBuf, PathBuf)>, AssetManagementError> {
    let mut companions = Vec::new();
    for (source, destination) in &plan {
        if !project.assets_root().join(source).is_file() {
            continue;
        }
        let Some(source_view) = crate::document::derive_view_path(source) else {
            continue;
        };
        if !project.assets_root().join(&source_view).is_file()
            || plan.iter().any(|(planned, _)| planned == &source_view)
        {
            continue;
        }
        let Some(destination_view) = crate::document::derive_view_path(destination) else {
            continue;
        };
        let destination_absolute = project.assets_root().join(&destination_view);
        if destination_absolute.exists() {
            return Err(AssetManagementError::DestinationExists(
                destination_absolute,
            ));
        }
        companions.push((source_view, destination_view));
    }
    plan.extend(companions);
    Ok(plan)
}

fn execute_move_plan(
    project: &ProjectRoot,
    manifest: &mut AssetManifest,
    plan: Vec<(PathBuf, PathBuf)>,
    is_trash: bool,
) -> Result<BatchAssetMoveReport, AssetManagementError> {
    let plan = expand_component_sidecars(project, plan)?;
    let plan = expand_graph_view_sidecars(project, plan)?;
    let touches_rust = plan.iter().any(|(source, destination)| {
        source.starts_with("scripts/rust") || destination.starts_with("scripts/rust")
    });
    let document_rewrites = if is_trash {
        Vec::new()
    } else {
        collect_document_rewrites(project, &plan)?
    };
    let original_manifest = manifest.clone();
    let mut applied = Vec::new();
    for (source, destination) in &plan {
        let source_absolute = project.assets_root().join(source);
        let destination_absolute = if is_trash {
            project.path().join(destination)
        } else {
            project.assets_root().join(destination)
        };
        if let Some(parent) = destination_absolute.parent() {
            std::fs::create_dir_all(parent).map_err(AssetManagementError::Io)?;
        }
        if let Err(error) = std::fs::rename(&source_absolute, &destination_absolute) {
            rollback_moves(project, &applied, is_trash)?;
            return Err(AssetManagementError::Io(error));
        }
        applied.push((source.clone(), destination.clone()));
    }

    if touches_rust
        && let Err(error) = refresh_game_module_indexes(project) {
            rollback_moves(project, &applied, is_trash)?;
            let _ = refresh_game_module_indexes(project);
            return Err(AssetManagementError::ScriptIndex(error.to_string()));
        }

    let mut affected = 0;
    let manifest_ids = manifest
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in manifest_ids {
        let Some(entry) = manifest.get_mut(&id) else {
            continue;
        };
        let entry_path = PathBuf::from(entry.path.replace('\\', "/"));
        let Some((source, destination)) = plan
            .iter()
            .find(|(source, _)| entry_path == *source || entry_path.starts_with(source))
        else {
            continue;
        };
        affected += 1;
        if is_trash {
            manifest.remove(&id);
        } else {
            let suffix = entry_path.strip_prefix(source).unwrap_or(Path::new(""));
            entry.path = normalize(&destination.join(suffix));
        }
    }
    let mut rewritten_documents: Vec<SerializedDocumentRewrite> = Vec::new();
    for rewrite in &document_rewrites {
        let path = project.assets_root().join(&rewrite.resulting_path);
        if let Err(error) = replace_file_contents(&path, &rewrite.updated) {
            for restored in rewritten_documents.iter().rev() {
                let _ = replace_file_contents(
                    &project.assets_root().join(&restored.resulting_path),
                    &restored.original,
                );
            }
            *manifest = original_manifest;
            rollback_moves(project, &applied, is_trash)?;
            if touches_rust {
                let _ = refresh_game_module_indexes(project);
            }
            return Err(AssetManagementError::Persist(error));
        }
        rewritten_documents.push(rewrite.clone());
    }
    if let Err(error) = persist_manifest(project, manifest) {
        for rewrite in rewritten_documents.iter().rev() {
            let _ = replace_file_contents(
                &project.assets_root().join(&rewrite.resulting_path),
                &rewrite.original,
            );
        }
        *manifest = original_manifest;
        rollback_moves(project, &applied, is_trash)?;
        if touches_rust {
            let _ = refresh_game_module_indexes(project);
        }
        return Err(error);
    }
    Ok(BatchAssetMoveReport {
        moves: applied
            .into_iter()
            .map(|(source, destination)| AssetMoveReport {
                source,
                destination,
                manifest_entries: 0,
            })
            .collect(),
        manifest_entries: affected,
        is_trash,
    })
}

#[derive(Clone)]
struct SerializedDocumentRewrite {
    original: String,
    updated: String,
    resulting_path: PathBuf,
}

fn collect_document_rewrites(
    project: &ProjectRoot,
    plan: &[(PathBuf, PathBuf)],
) -> Result<Vec<SerializedDocumentRewrite>, AssetManagementError> {
    let mut files = Vec::new();
    collect_supported_json_files(&project.assets_root(), &project.assets_root(), &mut files)?;
    let mut rewrites = Vec::new();
    for relative in files {
        let absolute = project.assets_root().join(&relative);
        let original = std::fs::read_to_string(&absolute).map_err(AssetManagementError::Io)?;
        let mut json: serde_json::Value = match serde_json::from_str(&original) {
            Ok(json) => json,
            Err(_) => continue,
        };
        if !rewrite_json_asset_paths(&mut json, plan) {
            continue;
        }
        let updated =
            serde_json::to_string_pretty(&json).map_err(AssetManagementError::ManifestJson)?;
        let resulting_path = remap_relative_path(&relative, plan);
        rewrites.push(SerializedDocumentRewrite {
            original,
            updated,
            resulting_path,
        });
    }
    Ok(rewrites)
}

fn collect_supported_json_files(
    assets_root: &Path,
    directory: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), AssetManagementError> {
    for entry in std::fs::read_dir(directory).map_err(AssetManagementError::Io)? {
        let entry = entry.map_err(AssetManagementError::Io)?;
        let file_type = entry.file_type().map_err(AssetManagementError::Io)?;
        if file_type.is_symlink() {
            return Err(AssetManagementError::SymlinkNotSupported(entry.path()));
        }
        if file_type.is_dir() {
            collect_supported_json_files(assets_root, &entry.path(), output)?;
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if [
            ".scene.json",
            ".ui.json",
            ".graph.json",
            ".prefab.json",
            ".material.json",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
            && let Ok(relative) = entry.path().strip_prefix(assets_root) {
                output.push(relative.to_path_buf());
            }
    }
    Ok(())
}

fn rewrite_json_asset_paths(value: &mut serde_json::Value, plan: &[(PathBuf, PathBuf)]) -> bool {
    match value {
        serde_json::Value::String(text) => {
            let prefix = text.starts_with("assets/").then_some("assets/");
            let candidate =
                PathBuf::from(prefix.map_or(text.as_str(), |prefix| &text[prefix.len()..]));
            let remapped = remap_relative_path(&candidate, plan);
            if remapped != candidate {
                *text = format!("{}{}", prefix.unwrap_or(""), normalize(&remapped));
                true
            } else {
                false
            }
        }
        serde_json::Value::Array(values) => {
            let mut changed = false;
            for value in values {
                changed = rewrite_json_asset_paths(value, plan) || changed;
            }
            changed
        }
        serde_json::Value::Object(values) => {
            let mut changed = false;
            for value in values.values_mut() {
                changed = rewrite_json_asset_paths(value, plan) || changed;
            }
            changed
        }
        _ => false,
    }
}

fn remap_relative_path(path: &Path, plan: &[(PathBuf, PathBuf)]) -> PathBuf {
    let Some((source, destination)) = plan
        .iter()
        .find(|(source, _)| path == source || path.starts_with(source))
    else {
        return path.to_path_buf();
    };
    destination.join(path.strip_prefix(source).unwrap_or(Path::new("")))
}

fn rollback_moves(
    project: &ProjectRoot,
    applied: &[(PathBuf, PathBuf)],
    is_trash: bool,
) -> Result<(), AssetManagementError> {
    let mut failures = Vec::new();
    for (source, destination) in applied.iter().rev() {
        let source_absolute = project.assets_root().join(source);
        let destination_absolute = if is_trash {
            project.path().join(destination)
        } else {
            project.assets_root().join(destination)
        };
        if std::fs::rename(&destination_absolute, &source_absolute).is_err() {
            failures.push(destination_absolute);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(AssetManagementError::RollbackFailed { paths: failures })
    }
}

fn validate_relative_or_root(path: &Path) -> Result<(), AssetManagementError> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    validate_relative(path)
}

fn ensure_no_symlink_ancestors(root: &Path, path: &Path) -> Result<(), AssetManagementError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| AssetManagementError::InvalidRelativePath(path.to_path_buf()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        if std::fs::symlink_metadata(&current)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(AssetManagementError::SymlinkNotSupported(current));
        }
    }
    Ok(())
}

fn ensure_tree_has_no_symlink(root: &Path) -> Result<(), AssetManagementError> {
    for entry in std::fs::read_dir(root).map_err(AssetManagementError::Io)? {
        let entry = entry.map_err(AssetManagementError::Io)?;
        let metadata = entry.metadata().map_err(AssetManagementError::Io)?;
        if entry
            .file_type()
            .map_err(AssetManagementError::Io)?
            .is_symlink()
        {
            return Err(AssetManagementError::SymlinkNotSupported(entry.path()));
        }
        if metadata.is_dir() {
            ensure_tree_has_no_symlink(&entry.path())?;
        }
    }
    Ok(())
}

impl std::error::Error for AssetManagementError {}

/// Renames or moves an asset within the project asset root.
pub fn move_asset(
    project: &ProjectRoot,
    manifest: &mut AssetManifest,
    source: &Path,
    destination: &Path,
) -> Result<AssetMoveReport, AssetManagementError> {
    let batch = move_asset_path(project, manifest, source, destination)?;
    let mut report =
        batch.moves.into_iter().next().ok_or_else(|| {
            AssetManagementError::MissingSource(project.assets_root().join(source))
        })?;
    report.manifest_entries = batch.manifest_entries;
    Ok(report)
}

/// Moves an asset into `.engine/asset_trash` and unregisters matching entries.
pub fn move_asset_to_trash(
    project: &ProjectRoot,
    manifest: &mut AssetManifest,
    source: &Path,
) -> Result<AssetMoveReport, AssetManagementError> {
    let batch = move_asset_paths_to_trash(project, manifest, &[source.to_path_buf()])?;
    let mut report =
        batch.moves.into_iter().next().ok_or_else(|| {
            AssetManagementError::MissingSource(project.assets_root().join(source))
        })?;
    report.manifest_entries = batch.manifest_entries;
    Ok(report)
}

/// One registered asset whose backing file is no longer on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanedAsset {
    /// Stable ID that scenes still reference.
    pub id: AssetId,
    /// Asset-root-relative path that no longer resolves to a file.
    pub path: PathBuf,
}

/// Lists registered assets whose files are missing from the asset root.
///
/// Deleting a file outside the editor leaves its manifest entry behind. The
/// entry is deliberately *not* removed here: dropping it would discard the
/// [`AssetId`] every scene reference is keyed by, so a file that comes back
/// (a branch switch, an undone move, a drive that was briefly offline) could
/// never be reconnected to its references. Callers report the result and let
/// the author decide, via [`unregister_orphaned_assets`], when a removal was
/// intentional.
pub fn orphaned_assets(project: &ProjectRoot, manifest: &AssetManifest) -> Vec<OrphanedAsset> {
    let assets_root = project.assets_root();
    manifest
        .iter()
        .filter(|(_, entry)| !assets_root.join(&entry.path).is_file())
        .map(|(id, entry)| OrphanedAsset {
            id: id.clone(),
            path: PathBuf::from(&entry.path),
        })
        .collect()
}

/// Unregisters missing manifest entries affected by one externally removed
/// asset file or directory.
///
/// The watcher supplies a path relative to the project asset root. An exact
/// file match is removed, while a directory match removes only entries below
/// that directory. Entries whose files have already returned are preserved.
/// Manifest persistence happens before the caller's in-memory manifest is
/// replaced, so a persistence failure leaves both representations unchanged.
pub fn unregister_removed_assets(
    project: &ProjectRoot,
    manifest: &mut AssetManifest,
    removed_path: &Path,
) -> Result<Vec<OrphanedAsset>, AssetManagementError> {
    // The watcher normally supplies a safe relative path. Validate it again at
    // this boundary so future callers cannot turn an event into a broad or
    // escaped manifest mutation.
    validate_relative(removed_path)?;

    // Manifest paths use forward slashes for stable serialization, while the
    // watcher uses the host path representation. Normalize both sides before
    // comparing them.
    let removed_path = PathBuf::from(normalize(removed_path));
    let assets_root = project.assets_root();

    // Match the removed file itself or a removed directory's descendants, but
    // only while the corresponding file is still absent. A file restored
    // before the debounced event is handled must keep its existing AssetId.
    let removed_assets = manifest
        .iter()
        .filter(|(_, entry)| {
            let entry_path = PathBuf::from(entry.path.replace('\\', "/"));
            let affected = entry_path == removed_path || entry_path.starts_with(&removed_path);
            affected && !assets_root.join(&entry.path).is_file()
        })
        .map(|(id, entry)| OrphanedAsset {
            id: id.clone(),
            path: PathBuf::from(&entry.path),
        })
        .collect::<Vec<_>>();

    // An unregistered file, a duplicate watcher event, or a file that has
    // already returned does not require a manifest write.
    if removed_assets.is_empty() {
        return Ok(removed_assets);
    }

    // Stage the change in a clone so the in-memory manifest remains consistent
    // with the on-disk file if canonical serialization or persistence fails.
    let mut updated = manifest.clone();
    for removed in &removed_assets {
        updated.remove(&removed.id);
    }

    // Persist first, then publish the new in-memory state to the editor.
    persist_manifest(project, &updated)?;
    *manifest = updated;

    Ok(removed_assets)
}

/// Removes every manifest entry whose file is missing and saves the manifest.
///
/// This is the deliberate counterpart to [`orphaned_assets`]. Scene references
/// to the removed IDs stay in their documents and are reported as unregistered
/// by scene validation, which is the same state a never-registered reference
/// would produce.
///
/// # Errors
///
/// Returns an error when the manifest cannot be written; the in-memory
/// manifest is left unchanged so it keeps matching the file on disk.
pub fn unregister_orphaned_assets(
    project: &ProjectRoot,
    manifest: &mut AssetManifest,
) -> Result<Vec<OrphanedAsset>, AssetManagementError> {
    let orphans = orphaned_assets(project, manifest);
    if orphans.is_empty() {
        return Ok(orphans);
    }
    let mut updated = manifest.clone();
    for orphan in &orphans {
        updated.remove(&orphan.id);
    }
    persist_manifest(project, &updated)?;
    *manifest = updated;
    Ok(orphans)
}

fn persist_manifest(
    project: &ProjectRoot,
    manifest: &AssetManifest,
) -> Result<(), AssetManagementError> {
    let json = manifest
        .to_canonical_json()
        .map_err(AssetManagementError::ManifestJson)?;
    replace_file_contents(&project.path().join("asset_manifest.json"), &json)
        .map_err(AssetManagementError::Persist)
}

fn validate_relative(path: &Path) -> Result<(), AssetManagementError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AssetManagementError::InvalidRelativePath(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

fn normalize(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            Component::CurDir => None,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{ImportSettings, ManifestEntry};
    use engine_authoring::{AssetId, ProjectConfig, PROJECT_SCHEMA_VERSION};

    fn project() -> (tempfile::TempDir, ProjectRoot) {
        let directory = tempfile::tempdir().unwrap();
        let root = ProjectRoot::create(
            directory.path(),
            ProjectConfig {
                name: "asset_ops".into(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .unwrap();
        (directory, root)
    }

    #[test]
    fn explorer_single_file_drop_copies_and_registers_asset() {
        let (_directory, project) = project();
        let external = tempfile::tempdir().unwrap();
        let source = external.path().join("player.obj");
        std::fs::write(&source, b"v 0 0 0\n").unwrap();
        let mut manifest = AssetManifest::default();

        let report = import_external_asset_files(
            &project,
            &mut manifest,
            std::slice::from_ref(&source),
            Path::new(""),
        )
        .unwrap();

        assert_eq!(report.registered.len(), 1);
        assert!(report.failures.is_empty());
        assert!(project.assets_root().join("player.obj").is_file());
        let imported = &report.registered[0];
        assert_eq!(manifest.get(&imported.asset_id).unwrap().path, "player.obj");
        let saved = std::fs::read_to_string(project.path().join("asset_manifest.json")).unwrap();
        let saved = AssetManifest::from_json(&saved).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved.get(&imported.asset_id).unwrap().path, "player.obj");
    }

    #[test]
    fn explorer_multiple_file_drop_registers_all_supported_files() {
        let (_directory, project) = project();
        let external = tempfile::tempdir().unwrap();
        let texture = external.path().join("icon.png");
        let audio = external.path().join("hit.wav");
        std::fs::write(&texture, b"png").unwrap();
        std::fs::write(&audio, b"wav").unwrap();
        let mut manifest = AssetManifest::default();

        let report =
            import_external_asset_files(&project, &mut manifest, &[texture, audio], Path::new(""))
                .unwrap();

        assert_eq!(report.registered.len(), 2);
        assert_eq!(manifest.len(), 2);
        assert!(project.assets_root().join("icon.png").is_file());
        assert!(project.assets_root().join("hit.wav").is_file());
    }

    #[test]
    fn external_drop_uses_selected_asset_folder() {
        let (_directory, project) = project();
        std::fs::create_dir_all(project.assets_root().join("characters")).unwrap();
        let external = tempfile::tempdir().unwrap();
        let source = external.path().join("player.obj");
        std::fs::write(&source, b"mesh").unwrap();
        let mut manifest = AssetManifest::default();

        let report = import_external_asset_files(
            &project,
            &mut manifest,
            &[source],
            Path::new("characters"),
        )
        .unwrap();

        assert_eq!(
            report.registered[0].destination,
            Path::new("characters/player.obj")
        );
        assert!(project
            .assets_root()
            .join("characters/player.obj")
            .is_file());
    }

    #[test]
    fn external_drop_collision_uses_suffix_without_overwriting() {
        let (_directory, project) = project();
        std::fs::write(project.assets_root().join("icon.png"), b"existing").unwrap();
        let external = tempfile::tempdir().unwrap();
        let source = external.path().join("icon.png");
        std::fs::write(&source, b"new").unwrap();
        let mut manifest = AssetManifest::default();

        let report =
            import_external_asset_files(&project, &mut manifest, &[source], Path::new("")).unwrap();

        assert_eq!(report.registered[0].destination, Path::new("icon_2.png"));
        assert_eq!(
            std::fs::read(project.assets_root().join("icon.png")).unwrap(),
            b"existing"
        );
        assert_eq!(
            std::fs::read(project.assets_root().join("icon_2.png")).unwrap(),
            b"new"
        );
    }

    #[test]
    fn external_drop_ignores_unsupported_format_without_copying() {
        let (_directory, project) = project();
        let external = tempfile::tempdir().unwrap();
        let source = external.path().join("notes.txt");
        std::fs::write(&source, b"notes").unwrap();
        let mut manifest = AssetManifest::default();

        let report =
            import_external_asset_files(&project, &mut manifest, &[source], Path::new("")).unwrap();

        assert!(report.registered.is_empty());
        assert!(report.failures.is_empty());
        assert!(manifest.is_empty());
        assert!(!project.assets_root().join("notes.txt").exists());
    }

    #[test]
    fn mixed_external_drop_imports_supported_and_ignores_unsupported_files() {
        let (_directory, project) = project();
        let external = tempfile::tempdir().unwrap();
        let supported = external.path().join("icon.png");
        let unsupported = external.path().join("notes.txt");
        std::fs::write(&supported, b"png").unwrap();
        std::fs::write(&unsupported, b"notes").unwrap();
        let mut manifest = AssetManifest::default();

        let report = import_external_asset_files(
            &project,
            &mut manifest,
            &[supported, unsupported],
            Path::new(""),
        )
        .unwrap();

        assert_eq!(report.registered.len(), 1);
        assert!(report.failures.is_empty());
        assert_eq!(manifest.len(), 1);
        assert!(project.assets_root().join("icon.png").is_file());
        assert!(!project.assets_root().join("notes.txt").exists());
    }

    #[test]
    fn external_folder_drop_preserves_supported_relative_paths() {
        let (_directory, project) = project();
        std::fs::create_dir_all(project.assets_root().join("meshes")).unwrap();
        let external = tempfile::tempdir().unwrap();
        let package = external.path().join("character_package");
        let textures = package.join("textures");
        let effects = package.join("PostAlphaEye");
        std::fs::create_dir_all(&textures).unwrap();
        std::fs::create_dir_all(&effects).unwrap();
        std::fs::write(package.join("character.pmx"), b"PMX ").unwrap();
        std::fs::write(textures.join("face.png"), b"png").unwrap();
        std::fs::write(package.join("readme.txt"), b"readme").unwrap();
        std::fs::write(effects.join("PostAlphaEye.fx"), b"effect").unwrap();
        let mut manifest = AssetManifest::default();

        let report = import_external_asset_files(
            &project,
            &mut manifest,
            &[package],
            Path::new("meshes"),
        )
        .unwrap();

        assert_eq!(report.registered.len(), 2);
        assert!(report.failures.is_empty());
        assert!(project
            .assets_root()
            .join("meshes/character_package/character.pmx")
            .is_file());
        assert!(project
            .assets_root()
            .join("meshes/character_package/textures/face.png")
            .is_file());
        assert!(!project
            .assets_root()
            .join("meshes/character_package/readme.txt")
            .exists());
        assert!(!project
            .assets_root()
            .join("meshes/character_package/PostAlphaEye")
            .exists());

        let paths = manifest
            .iter()
            .map(|(_, entry)| entry.path.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            paths,
            std::collections::BTreeSet::from([
                "meshes/character_package/character.pmx",
                "meshes/character_package/textures/face.png",
            ])
        );
    }

    #[test]
    fn external_drop_reports_copy_failure() {
        let (_directory, project) = project();
        let missing = project.path().join("missing.png");
        let mut manifest = AssetManifest::default();

        let report =
            import_external_asset_files(&project, &mut manifest, &[missing], Path::new(""))
                .unwrap();

        assert!(report.registered.is_empty());
        assert!(matches!(
            report.failures[0].kind,
            ExternalAssetImportFailureKind::CopyFailed(_)
        ));
        assert!(manifest.is_empty());
    }

    #[test]
    fn manifest_save_failure_rolls_back_external_copies_and_registration() {
        let (_directory, project) = project();
        let manifest_path = project.path().join("asset_manifest.json");
        if manifest_path.is_file() {
            std::fs::remove_file(&manifest_path).unwrap();
        }
        std::fs::create_dir(&manifest_path).unwrap();
        let external = tempfile::tempdir().unwrap();
        let source = external.path().join("icon.png");
        std::fs::write(&source, b"png").unwrap();
        let mut manifest = AssetManifest::default();

        let result = import_external_asset_files(&project, &mut manifest, &[source], Path::new(""));

        assert!(result.is_err());
        assert!(manifest.is_empty());
        assert!(!project.assets_root().join("icon.png").exists());
    }

    #[test]
    fn manifest_save_failure_removes_recursively_created_import_folders() {
        let (_directory, project) = project();
        let manifest_path = project.path().join("asset_manifest.json");
        if manifest_path.is_file() {
            std::fs::remove_file(&manifest_path).unwrap();
        }
        std::fs::create_dir(&manifest_path).unwrap();
        let external = tempfile::tempdir().unwrap();
        let package = external.path().join("package");
        std::fs::create_dir_all(package.join("textures")).unwrap();
        std::fs::write(package.join("character.pmx"), b"PMX ").unwrap();
        std::fs::write(package.join("textures/face.png"), b"png").unwrap();
        let mut manifest = AssetManifest::default();

        let result =
            import_external_asset_files(&project, &mut manifest, &[package], Path::new(""));

        assert!(result.is_err());
        assert!(manifest.is_empty());
        assert!(!project.assets_root().join("package").exists());
    }

    #[test]
    fn move_updates_file_and_manifest_path() {
        let (_directory, project) = project();
        std::fs::create_dir_all(project.assets_root().join("textures")).unwrap();
        std::fs::write(project.assets_root().join("textures/a.png"), b"png").unwrap();
        let id = AssetId::generate();
        let mut manifest = AssetManifest::default();
        manifest.insert(
            id.clone(),
            ManifestEntry {
                path: "textures/a.png".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );

        move_asset(
            &project,
            &mut manifest,
            Path::new("textures/a.png"),
            Path::new("ui/a.png"),
        )
        .unwrap();
        assert!(project.assets_root().join("ui/a.png").is_file());
        assert_eq!(manifest.get(&id).unwrap().path, "ui/a.png");
    }

    #[test]
    fn moving_a_graph_carries_its_hidden_view_sidecar() {
        let (_directory, project) = project();
        std::fs::create_dir_all(project.assets_root().join("graphs")).unwrap();
        std::fs::create_dir_all(project.assets_root().join("organized")).unwrap();
        std::fs::write(
            project
                .assets_root()
                .join("graphs/player.anim.graph.json"),
            b"{}",
        )
        .unwrap();
        std::fs::write(
            project
                .assets_root()
                .join("graphs/player.anim.graph.view.json"),
            b"{}",
        )
        .unwrap();
        let id = AssetId::generate();
        let mut manifest = AssetManifest::default();
        manifest.insert(
            id.clone(),
            ManifestEntry {
                path: "graphs/player.anim.graph.json".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );

        move_asset(
            &project,
            &mut manifest,
            Path::new("graphs/player.anim.graph.json"),
            Path::new("organized/player.anim.graph.json"),
        )
        .unwrap();

        assert!(project
            .assets_root()
            .join("organized/player.anim.graph.json")
            .is_file());
        assert!(project
            .assets_root()
            .join("organized/player.anim.graph.view.json")
            .is_file());
        assert!(!project
            .assets_root()
            .join("graphs/player.anim.graph.view.json")
            .exists());
        assert_eq!(
            manifest.get(&id).unwrap().path,
            "organized/player.anim.graph.json"
        );
    }

    #[test]
    fn trashing_a_graph_carries_its_hidden_view_sidecar() {
        let (_directory, project) = project();
        std::fs::write(
            project.assets_root().join("enemy.anim.graph.json"),
            b"{}",
        )
        .unwrap();
        std::fs::write(
            project.assets_root().join("enemy.anim.graph.view.json"),
            b"{}",
        )
        .unwrap();
        let id = AssetId::generate();
        let mut manifest = AssetManifest::default();
        manifest.insert(
            id.clone(),
            ManifestEntry {
                path: "enemy.anim.graph.json".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );

        let report = move_asset_paths_to_trash(
            &project,
            &mut manifest,
            &[PathBuf::from("enemy.anim.graph.json")],
        )
        .unwrap();

        assert_eq!(report.moves.len(), 2);
        assert!(report.moves.iter().any(|moved| {
            moved.source == Path::new("enemy.anim.graph.view.json")
                && project.path().join(&moved.destination).is_file()
        }));
        assert!(!project
            .assets_root()
            .join("enemy.anim.graph.view.json")
            .exists());
        assert!(manifest.get(&id).is_none());
    }

    #[test]
    fn trash_is_recoverable_and_unregisters_asset() {
        let (_directory, project) = project();
        std::fs::write(project.assets_root().join("old.rhai"), b"fn update() {}").unwrap();
        let id = AssetId::generate();
        let mut manifest = AssetManifest::default();
        manifest.insert(
            id.clone(),
            ManifestEntry {
                path: "old.rhai".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );
        let report = move_asset_to_trash(&project, &mut manifest, Path::new("old.rhai")).unwrap();
        assert!(project.path().join(report.destination).is_file());
        assert!(manifest.get(&id).is_none());
    }

    #[test]
    fn external_removal_unregisters_only_the_removed_asset_and_persists() {
        let (_directory, project) = project();
        std::fs::write(project.assets_root().join("removed.pmx"), b"PMX ").unwrap();
        std::fs::write(project.assets_root().join("kept.vmd"), b"VMD ").unwrap();
        let removed_id = AssetId::generate();
        let kept_id = AssetId::generate();
        let mut manifest = AssetManifest::default();
        for (id, path) in [(&removed_id, "removed.pmx"), (&kept_id, "kept.vmd")] {
            manifest.insert(
                id.clone(),
                ManifestEntry {
                    path: path.into(),
                    name: None,
                    import_settings: ImportSettings::default(),
                },
            );
        }

        // Reproduce a file removed outside the editor while the watcher is
        // able to identify the original asset-relative path.
        std::fs::remove_file(project.assets_root().join("removed.pmx")).unwrap();

        let removed =
            unregister_removed_assets(&project, &mut manifest, Path::new("removed.pmx")).unwrap();

        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].id, removed_id);
        assert!(manifest.get(&removed_id).is_none());
        assert!(manifest.get(&kept_id).is_some());

        let saved = AssetManifest::from_json(
            &std::fs::read_to_string(project.path().join("asset_manifest.json")).unwrap(),
        )
        .unwrap();
        assert!(saved.get(&removed_id).is_none());
        assert!(saved.get(&kept_id).is_some());
    }

    #[test]
    fn a_restored_file_stops_being_reported_with_its_id_intact() {
        let (_directory, project) = project();
        let path = project.assets_root().join("prop.obj");
        std::fs::write(&path, b"v 0 0 0\n").unwrap();
        let id = AssetId::generate();
        let mut manifest = AssetManifest::default();
        manifest.insert(
            id.clone(),
            ManifestEntry {
                path: "prop.obj".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );

        // A branch switch or an undone move puts the file back.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(orphaned_assets(&project, &manifest).len(), 1);
        std::fs::write(&path, b"v 0 0 0\n").unwrap();

        assert!(orphaned_assets(&project, &manifest).is_empty());
        assert_eq!(manifest.get(&id).unwrap().path, "prop.obj");
    }

    #[test]
    fn unregistering_missing_assets_removes_only_them_and_persists() {
        let (_directory, project) = project();
        std::fs::write(project.assets_root().join("kept.obj"), b"v 0 0 0\n").unwrap();
        std::fs::write(project.assets_root().join("gone.obj"), b"v 0 0 0\n").unwrap();
        let kept = AssetId::generate();
        let gone = AssetId::generate();
        let mut manifest = AssetManifest::default();
        for (id, path) in [(&kept, "kept.obj"), (&gone, "gone.obj")] {
            manifest.insert(
                id.clone(),
                ManifestEntry {
                    path: path.into(),
                    name: None,
                    import_settings: ImportSettings::default(),
                },
            );
        }
        std::fs::remove_file(project.assets_root().join("gone.obj")).unwrap();

        let removed = unregister_orphaned_assets(&project, &mut manifest).unwrap();

        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].id, gone);
        assert!(manifest.get(&gone).is_none());
        assert!(manifest.get(&kept).is_some());
        let saved = AssetManifest::from_json(
            &std::fs::read_to_string(project.path().join("asset_manifest.json")).unwrap(),
        )
        .unwrap();
        assert!(saved.get(&gone).is_none());
        assert!(saved.get(&kept).is_some());
    }

    #[test]
    fn folder_move_preserves_nested_manifest_identity() {
        let (_directory, project) = project();
        std::fs::create_dir_all(project.assets_root().join("gameplay/coins")).unwrap();
        std::fs::create_dir_all(project.assets_root().join("organized")).unwrap();
        std::fs::write(
            project.assets_root().join("gameplay/coins/coin.png"),
            b"png",
        )
        .unwrap();
        let id = AssetId::generate();
        let mut manifest = AssetManifest::default();
        manifest.insert(
            id.clone(),
            ManifestEntry {
                path: "gameplay/coins/coin.png".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );

        let report = move_asset_batch(
            &project,
            &mut manifest,
            &[PathBuf::from("gameplay/coins")],
            Path::new("organized"),
        )
        .unwrap();

        assert_eq!(report.manifest_entries, 1);
        assert!(project
            .assets_root()
            .join("organized/coins/coin.png")
            .is_file());
        assert_eq!(manifest.get(&id).unwrap().path, "organized/coins/coin.png");
    }

    #[test]
    fn move_rewrites_supported_serialized_asset_references() {
        let (_directory, project) = project();
        std::fs::create_dir_all(project.assets_root().join("textures")).unwrap();
        std::fs::create_dir_all(project.assets_root().join("organized")).unwrap();
        std::fs::write(project.assets_root().join("textures/a.png"), b"png").unwrap();
        std::fs::write(
            project.assets_root().join("hud.ui.json"),
            r#"{
  "texture": "assets/textures/a.png",
  "relative_texture": "textures/a.png",
  "unrelated": "textures/another.png"
}"#,
        )
        .unwrap();
        let mut manifest = AssetManifest::default();

        move_asset(
            &project,
            &mut manifest,
            Path::new("textures/a.png"),
            Path::new("organized/a.png"),
        )
        .unwrap();

        let document = std::fs::read_to_string(project.assets_root().join("hud.ui.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&document).unwrap();
        assert_eq!(json["texture"], "assets/organized/a.png");
        assert_eq!(json["relative_texture"], "organized/a.png");
        assert_eq!(json["unrelated"], "textures/another.png");
    }

    fn module_index(project: &ProjectRoot) -> String {
        std::fs::read_to_string(project.game_module_index_path()).unwrap()
    }

    fn create_script(
        project: &ProjectRoot,
        kind: engine_authoring::RustScriptKind,
        name: &str,
    ) -> PathBuf {
        engine_authoring::create_rust_script(
            project,
            kind,
            name,
            engine_authoring::RustScriptSchedule::Update,
        )
        .unwrap()
    }

    #[test]
    fn component_moves_to_a_feature_folder_and_carries_its_sidecar() {
        let (_directory, project) = project();
        let source = create_script(
            &project,
            engine_authoring::RustScriptKind::Component,
            "Health",
        );
        let component_id =
            engine_authoring::load_component_metadata(&component_metadata_path(&source))
                .unwrap()
                .component_id;
        let mut manifest = AssetManifest::default();

        move_asset(
            &project,
            &mut manifest,
            Path::new("scripts/rust/components/health.rs"),
            Path::new("scripts/rust/player/health.rs"),
        )
        .unwrap();

        assert!(!source.exists());
        let moved = project.assets_root().join("scripts/rust/player/health.rs");
        assert!(moved.is_file());
        assert!(project
            .assets_root()
            .join("scripts/rust/player/health.rs.meta.json")
            .is_file());
        assert_eq!(
            engine_authoring::load_component_metadata(&component_metadata_path(&moved))
                .unwrap()
                .component_id,
            component_id,
            "moving a component must never change its stable ID"
        );
        let index = module_index(&project);
        assert!(index.contains("pub mod player {"));
        assert!(index.contains("scripts/rust/player/health.rs"));
        assert!(!index.contains("scripts/rust/components/health.rs"));
    }

    #[test]
    fn every_rust_script_kind_moves_out_of_its_recommended_folder() {
        let (_directory, project) = project();
        create_script(
            &project,
            engine_authoring::RustScriptKind::Resource,
            "MissionState",
        );
        create_script(
            &project,
            engine_authoring::RustScriptKind::System,
            "PlayerMove",
        );
        create_script(
            &project,
            engine_authoring::RustScriptKind::SharedModule,
            "Math",
        );
        let mut manifest = AssetManifest::default();

        for (source, destination) in [
            (
                "scripts/rust/resources/mission_state.rs",
                "scripts/rust/gameplay/state.rs",
            ),
            (
                "scripts/rust/systems/player_move.rs",
                "scripts/rust/player/movement.rs",
            ),
            ("scripts/rust/shared/math.rs", "scripts/rust/common/math.rs"),
        ] {
            move_asset(
                &project,
                &mut manifest,
                Path::new(source),
                Path::new(destination),
            )
            .unwrap_or_else(|error| panic!("moving {source} must succeed: {error}"));
            assert!(project.assets_root().join(destination).is_file());
        }

        let index = module_index(&project);
        assert!(index.contains("pub mod gameplay {"));
        assert!(index.contains("pub mod state;"));
        assert!(index.contains("pub mod movement;"));
        assert!(index.contains("pub mod common {"));
        assert!(!index.contains("scripts/rust/systems/player_move.rs"));
    }

    #[test]
    fn rust_sources_may_not_leave_the_rust_script_root() {
        let (_directory, project) = project();
        create_script(
            &project,
            engine_authoring::RustScriptKind::Component,
            "Health",
        );
        let mut manifest = AssetManifest::default();

        let error = move_asset(
            &project,
            &mut manifest,
            Path::new("scripts/rust/components/health.rs"),
            Path::new("meshes/health.rs"),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            AssetManagementError::ScriptMoveRestricted { .. }
        ));
        assert!(project
            .assets_root()
            .join("scripts/rust/components/health.rs")
            .is_file());
    }

    #[test]
    fn rust_destinations_must_be_valid_module_names() {
        let (_directory, project) = project();
        create_script(
            &project,
            engine_authoring::RustScriptKind::Component,
            "Health",
        );
        let mut manifest = AssetManifest::default();

        for destination in [
            "scripts/rust/player-movement/health.rs",
            "scripts/rust/player/hit points.rs",
        ] {
            let error = move_asset(
                &project,
                &mut manifest,
                Path::new("scripts/rust/components/health.rs"),
                Path::new(destination),
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    AssetManagementError::ScriptPlacementRestricted { .. }
                ),
                "{destination} must be rejected as a Rust module path"
            );
        }
        assert!(project
            .assets_root()
            .join("scripts/rust/components/health.rs")
            .is_file());
    }

    #[test]
    fn shared_rust_modules_can_be_moved_and_trashed_like_any_other_source() {
        let (_directory, project) = project();
        create_script(
            &project,
            engine_authoring::RustScriptKind::SharedModule,
            "MathHelpers",
        );
        let mut manifest = AssetManifest::default();

        move_asset(
            &project,
            &mut manifest,
            Path::new("scripts/rust/shared/math_helpers.rs"),
            Path::new("scripts/rust/shared/util/math_helpers.rs"),
        )
        .unwrap();
        assert!(module_index(&project).contains("pub mod util {"));

        move_asset_to_trash(
            &project,
            &mut manifest,
            Path::new("scripts/rust/shared/util/math_helpers.rs"),
        )
        .unwrap();
        assert!(!module_index(&project).contains("math_helpers"));
    }

    #[test]
    fn a_failed_index_refresh_restores_every_moved_rust_file() {
        let (_directory, project) = project();
        let source = create_script(
            &project,
            engine_authoring::RustScriptKind::Component,
            "Health",
        );
        // A colliding module name is only detectable after the move, so the
        // rollback path must return both the source and its sidecar.
        std::fs::create_dir_all(project.assets_root().join("scripts/rust/player/health")).unwrap();
        let mut manifest = AssetManifest::default();

        let error = move_asset(
            &project,
            &mut manifest,
            Path::new("scripts/rust/components/health.rs"),
            Path::new("scripts/rust/player/health.rs"),
        )
        .unwrap_err();

        assert!(matches!(error, AssetManagementError::ScriptIndex(_)));
        assert!(source.is_file());
        assert!(component_metadata_path(&source).is_file());
        assert!(!project
            .assets_root()
            .join("scripts/rust/player/health.rs")
            .exists());
    }

    #[test]
    fn manifest_save_failure_rolls_back_a_rust_move_completely() {
        let (_directory, project) = project();
        let source = create_script(
            &project,
            engine_authoring::RustScriptKind::Component,
            "Health",
        );
        let manifest_path = project.path().join("asset_manifest.json");
        if manifest_path.is_file() {
            std::fs::remove_file(&manifest_path).unwrap();
        }
        // A directory in the manifest's place makes atomic persistence fail
        // after the files and the generated index have already been updated.
        std::fs::create_dir(&manifest_path).unwrap();
        let mut manifest = AssetManifest::default();

        let error = move_asset(
            &project,
            &mut manifest,
            Path::new("scripts/rust/components/health.rs"),
            Path::new("scripts/rust/player/health.rs"),
        )
        .unwrap_err();

        assert!(matches!(error, AssetManagementError::Persist(_)));
        assert!(source.is_file());
        assert!(component_metadata_path(&source).is_file());
        assert!(!project
            .assets_root()
            .join("scripts/rust/player/health.rs")
            .exists());
        assert!(module_index(&project).contains("scripts/rust/components/health.rs"));
    }

    #[test]
    fn a_rust_folder_moves_with_every_source_and_sidecar_inside_it() {
        let (_directory, project) = project();
        engine_authoring::create_rust_script_in(
            &project,
            engine_authoring::RustScriptKind::Component,
            Path::new("components/enemies"),
            "Health",
            engine_authoring::RustScriptSchedule::Update,
        )
        .unwrap();
        std::fs::create_dir_all(project.assets_root().join("scripts/rust/enemy")).unwrap();
        let mut manifest = AssetManifest::default();

        move_asset_batch(
            &project,
            &mut manifest,
            &[PathBuf::from("scripts/rust/components/enemies")],
            Path::new("scripts/rust/enemy"),
        )
        .unwrap();

        assert!(project
            .assets_root()
            .join("scripts/rust/enemy/enemies/health.rs")
            .is_file());
        assert!(project
            .assets_root()
            .join("scripts/rust/enemy/enemies/health.rs.meta.json")
            .is_file());
        assert!(module_index(&project).contains("scripts/rust/enemy/enemies/health.rs"));
    }

    #[test]
    fn overlapping_folder_selection_is_rejected_before_any_move() {
        let (_directory, project) = project();
        std::fs::create_dir_all(project.assets_root().join("source/child")).unwrap();
        std::fs::create_dir_all(project.assets_root().join("destination")).unwrap();
        std::fs::write(project.assets_root().join("source/child/a.txt"), b"a").unwrap();
        let mut manifest = AssetManifest::default();

        let error = move_asset_batch(
            &project,
            &mut manifest,
            &[PathBuf::from("source"), PathBuf::from("source/child")],
            Path::new("destination"),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            AssetManagementError::OverlappingSources { .. }
        ));
        assert!(project.assets_root().join("source/child/a.txt").is_file());
        assert!(!project.assets_root().join("destination/source").exists());
    }

    #[test]
    fn collision_preflight_leaves_every_source_unchanged() {
        let (_directory, project) = project();
        std::fs::create_dir_all(project.assets_root().join("one")).unwrap();
        std::fs::create_dir_all(project.assets_root().join("two")).unwrap();
        std::fs::create_dir_all(project.assets_root().join("destination")).unwrap();
        std::fs::write(project.assets_root().join("one/a.txt"), b"one").unwrap();
        std::fs::write(project.assets_root().join("two/a.txt"), b"two").unwrap();
        let mut manifest = AssetManifest::default();

        let error = move_asset_batch(
            &project,
            &mut manifest,
            &[PathBuf::from("one/a.txt"), PathBuf::from("two/a.txt")],
            Path::new("destination"),
        )
        .unwrap_err();

        assert!(matches!(error, AssetManagementError::DestinationExists(_)));
        assert!(project.assets_root().join("one/a.txt").is_file());
        assert!(project.assets_root().join("two/a.txt").is_file());
    }

    #[test]
    fn trash_preflight_failure_does_not_create_a_trash_directory() {
        let (_directory, project) = project();
        let mut manifest = AssetManifest::default();

        let error =
            move_asset_paths_to_trash(&project, &mut manifest, &[PathBuf::from("missing.txt")])
                .unwrap_err();

        assert!(matches!(error, AssetManagementError::MissingSource(_)));
        assert!(!project.path().join(".engine/asset_trash").exists());
    }
}
