//! Build / packaging analysis (Phase 39, ADR 0034).
//!
//! `analyze_build` performs reachability and read-only source validation and
//! returns a `BuildReport` with included assets and diagnostics. The actual
//! `cargo build --release` invocation is in `build_project`, which is NOT
//! unit-tested (it requires a real toolchain).

use engine::AssetManifest;
use engine_authoring::id::AssetId;
use engine_authoring::StableId;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Component;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for a single build operation.
pub struct BuildConfig {
    /// Root directory of the editor project.
    pub project_root: PathBuf,
    /// Directory that receives the packaged output.
    pub output_dir: PathBuf,
    /// Relative path of the start scene (from `project_settings.json`).
    pub start_scene: Option<String>,
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Category of a build diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildDiagnosticKind {
    /// No start scene is configured; the build cannot proceed.
    MissingStartScene,
    /// A manifest entry references a file that does not exist on disk.
    MissingAsset {
        /// Relative path of the missing asset.
        path: String,
    },
    /// An imported glTF sidecar does not exist on disk.
    MissingSourceDependency {
        /// Source-relative dependency path.
        path: String,
    },
    /// Imported metadata contains a malformed stable sub-asset ID.
    InvalidImportedAssetId {
        /// Persisted ID string.
        id: String,
    },
    /// A sidecar path is absolute or attempts to escape `assets/`.
    InvalidSourceDependency {
        /// Rejected dependency path.
        path: String,
    },
    /// Source content changed after the last successful import.
    StaleImportedSource {
        /// Registered source path.
        path: String,
    },
    /// A registered `*.retarget.json` map's source or target skeleton could
    /// not be located among the manifest's model sources (glTF/GLB or FBX,
    /// ADR 0079 §4 / ADR 0081).
    RetargetSkeletonUnresolved {
        /// Registered map path.
        path: String,
    },
    /// Baking a retargeted clip for a registered map failed.
    RetargetBakeFailed {
        /// Registered map path.
        path: String,
        /// Failure detail from [`engine::retarget_clip`] or the bake I/O.
        reason: String,
    },
    /// A registered map's source clip belongs to a source with no recorded
    /// `source_fingerprint` (AP-6). Baking under a same-session-only
    /// fallback key would poison the on-disk derived-clip cache across
    /// builds, so packaging refuses to stage the clip until the source is
    /// reimported and a fingerprint is recorded.
    RetargetSourceUnfingerprinted {
        /// Registered map path.
        path: String,
        /// Sub-asset ID of the animation clip that could not be baked.
        clip: String,
    },
    /// A registered `*.retarget.json` map's `(source_skeleton,
    /// target_skeleton)` pair is not needed by any entity the AP-7
    /// reachability trace found (a unified `engine.animation_controller` or
    /// legacy animator/skeleton pair in a scanned scene or prefab), and the map does not set
    /// `always_package`. Its clips are skipped rather than baked.
    ///
    /// Non-blocking: unlike an unresolved skeleton or a failed bake, an
    /// unreached map is not a hole in the package, just a narrowing the
    /// author can act on (delete the map, reference it from content, or set
    /// `always_package` for a runtime-only usage the static trace cannot
    /// see).
    RetargetMapNotReached {
        /// Registered map path.
        path: String,
    },
    /// Pre-baking or staging a skeleton-independent Humanoid motion failed.
    HumanoidBakeFailed {
        /// Stable HumanoidMotion sub-asset ID.
        motion: String,
        /// Target skeleton ID when the failure happened during target bake.
        target_skeleton: Option<String>,
        /// Import, validation, cache, or bake failure detail.
        reason: String,
    },
}

/// A single diagnostic produced during build analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildDiagnostic {
    /// Category of this diagnostic.
    pub kind: BuildDiagnosticKind,
    /// Human-readable description.
    pub message: String,
    /// Whether this diagnostic blocks the build from running.
    pub blocking: bool,
}

impl BuildDiagnostic {
    fn blocking(kind: BuildDiagnosticKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            blocking: true,
        }
    }

    fn non_blocking(kind: BuildDiagnosticKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            blocking: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// Result of a build analysis pass.
pub struct BuildReport {
    /// Asset IDs that will be included in the package.
    pub reachable_assets: BTreeSet<AssetId>,
    /// Diagnostics produced during analysis.
    pub diagnostics: Vec<BuildDiagnostic>,
    /// `true` when no blocking diagnostics were found.
    pub success: bool,
}

// ---------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------

/// Performs a pure reachability analysis and returns a [`BuildReport`].
///
/// In v1, the reachability policy is conservative: all manifest entries are
/// considered reachable.  Fine-grained scene-file traversal is deferred.
pub fn analyze_build(config: &BuildConfig, manifest: &AssetManifest) -> BuildReport {
    let mut diagnostics = Vec::new();

    if config.start_scene.is_none() {
        diagnostics.push(BuildDiagnostic::blocking(
            BuildDiagnosticKind::MissingStartScene,
            "no start scene is configured in project settings; build cannot proceed",
        ));
        return BuildReport {
            reachable_assets: BTreeSet::new(),
            diagnostics,
            success: false,
        };
    }

    let mut reachable_assets: BTreeSet<AssetId> =
        manifest.iter().map(|(id, _)| id.clone()).collect();

    let assets_root = config.project_root.join("assets");
    for (source_id, entry) in manifest.iter() {
        let abs = assets_root.join(&entry.path);
        if !abs.exists() {
            diagnostics.push(BuildDiagnostic::non_blocking(
                BuildDiagnosticKind::MissingAsset {
                    path: entry.path.clone(),
                },
                format!(
                    "asset '{}' is listed in the manifest but not found on disk",
                    entry.path
                ),
            ));
        }
        for sub_asset in &entry.import_settings.sub_assets {
            match (
                AssetId::from_stable_id(StableId::new(&sub_asset.id)),
                engine::asset::expected_imported_sub_asset_id(source_id, sub_asset),
            ) {
                (Ok(id), Ok(expected)) if id == expected => {
                    reachable_assets.insert(id);
                }
                _ => diagnostics.push(BuildDiagnostic::blocking(
                    BuildDiagnosticKind::InvalidImportedAssetId {
                        id: sub_asset.id.clone(),
                    },
                    format!(
                        "source '{}' contains invalid imported sub-asset ID '{}'",
                        entry.path, sub_asset.id
                    ),
                )),
            }
        }
        let mut dependency_paths = Vec::new();
        let mut dependencies_present = abs.is_file();
        for dependency in &entry.import_settings.source_dependencies {
            if !is_safe_asset_relative_path(dependency) {
                diagnostics.push(BuildDiagnostic::blocking(
                    BuildDiagnosticKind::InvalidSourceDependency {
                        path: dependency.clone(),
                    },
                    format!(
                        "source '{}' declares dependency '{}' outside the assets root",
                        entry.path, dependency
                    ),
                ));
                dependencies_present = false;
                continue;
            }
            let dependency_path = assets_root.join(dependency);
            if !dependency_path.is_file() {
                dependencies_present = false;
                diagnostics.push(BuildDiagnostic::non_blocking(
                    BuildDiagnosticKind::MissingSourceDependency {
                        path: dependency.clone(),
                    },
                    format!(
                        "source '{}' requires dependency '{}' which is not found on disk",
                        entry.path, dependency
                    ),
                ));
            }
            dependency_paths.push(dependency_path);
        }
        if dependencies_present
            && let Some(expected) = &entry.import_settings.source_fingerprint {
                match engine::fingerprint_model_source(&abs, &dependency_paths) {
                    Ok(actual) if &actual != expected => {
                        diagnostics.push(BuildDiagnostic::blocking(
                            BuildDiagnosticKind::StaleImportedSource {
                                path: entry.path.clone(),
                            },
                            format!(
                                "source '{}' changed after its last import; reimport before building",
                                entry.path
                            ),
                        ));
                    }
                    Ok(_) => {}
                    Err(error) => diagnostics.push(BuildDiagnostic::blocking(
                        BuildDiagnosticKind::StaleImportedSource {
                            path: entry.path.clone(),
                        },
                        format!(
                            "source '{}' could not be fingerprinted: {error}",
                            entry.path
                        ),
                    )),
                }
            }
    }

    let success = !diagnostics.iter().any(|d| d.blocking);
    BuildReport {
        reachable_assets,
        diagnostics,
        success,
    }
}

// ---------------------------------------------------------------------------
// Build invocation (not unit-tested)
// ---------------------------------------------------------------------------

/// Errors that can occur during the build invocation.
#[derive(Debug)]
pub enum BuildError {
    /// The analysis found blocking diagnostics.
    AnalysisFailed(Vec<BuildDiagnostic>),
    /// `cargo build --release` could not be launched or returned non-zero.
    CargoFailed(std::io::Error),
    /// `cargo build` exited with a non-zero status.
    CargoExitFailure {
        /// Exit code returned by cargo.
        code: Option<i32>,
    },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AnalysisFailed(_) => write!(f, "build analysis produced blocking diagnostics"),
            Self::CargoFailed(e) => write!(f, "cargo build failed to launch: {e}"),
            Self::CargoExitFailure { code: Some(c) } => {
                write!(f, "cargo build exited with code {c}")
            }
            Self::CargoExitFailure { code: None } => {
                write!(f, "cargo build terminated by signal")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// Invokes `cargo build --release` for `config.project_root`.
///
/// This function spawns a subprocess and is **not unit-tested**.  It is
/// intended to be called from the editor UI after `analyze_build` succeeds.
///
/// # Errors
///
/// Returns [`BuildError::AnalysisFailed`] when the analysis has blocking
/// diagnostics.  Returns [`BuildError::CargoFailed`] or
/// [`BuildError::CargoExitFailure`] when cargo cannot be launched or fails.
pub fn build_project(
    config: &BuildConfig,
    manifest: &AssetManifest,
) -> Result<BuildReport, BuildError> {
    let report = analyze_build(config, manifest);
    if !report.success {
        let blocking: Vec<BuildDiagnostic> = report
            .diagnostics
            .into_iter()
            .filter(|d| d.blocking)
            .collect();
        return Err(BuildError::AnalysisFailed(blocking));
    }

    let manifest_path = config.project_root.join("Cargo.toml");
    let status = std::process::Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .status()
        .map_err(BuildError::CargoFailed)?;

    if !status.success() {
        return Err(BuildError::CargoExitFailure {
            code: status.code(),
        });
    }

    Ok(BuildReport {
        reachable_assets: report.reachable_assets,
        diagnostics: report.diagnostics,
        success: true,
    })
}

// ---------------------------------------------------------------------------
// Packaging (Phase 51, ADR 0045)
// ---------------------------------------------------------------------------

/// One file copy in a package plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCopy {
    /// Source path relative to the project root.
    pub source: PathBuf,
    /// Destination path relative to the output directory.
    pub destination: PathBuf,
}

/// Result of package planning.
pub struct PackagePlan {
    /// Files to copy from the project into the package.
    pub copies: Vec<PackageCopy>,
    /// Diagnostics produced during planning.
    pub diagnostics: Vec<BuildDiagnostic>,
    /// `true` when no blocking diagnostics were found.
    pub success: bool,
}

/// Plans the package layout for `config` (ADR 0045).
///
/// Reuses the ADR 0034 analysis but escalates [`BuildDiagnosticKind::MissingAsset`]
/// to blocking: packaging refuses to produce a package with holes. The copy
/// list covers `project.json`, `project_settings.json`, every authored scene,
/// and every manifest asset with its directory structure preserved;
/// `asset_manifest.json` is re-serialized by [`package_project`] rather than
/// copied. Copying every scene is intentionally conservative: game modules can
/// request a scene switch at runtime, so packaging only the configured start
/// scene would produce a package that starts successfully but fails later.
pub fn plan_package(config: &BuildConfig, manifest: &AssetManifest) -> PackagePlan {
    let report = analyze_build(config, manifest);
    let mut diagnostics = report.diagnostics;
    for diagnostic in &mut diagnostics {
        if matches!(
            diagnostic.kind,
            BuildDiagnosticKind::MissingAsset { .. }
                | BuildDiagnosticKind::MissingSourceDependency { .. }
        ) {
            diagnostic.blocking = true;
        }
    }
    let success = !diagnostics.iter().any(|diagnostic| diagnostic.blocking);

    let mut copies = Vec::new();
    if success {
        copies.push(PackageCopy {
            source: PathBuf::from("project.json"),
            destination: PathBuf::from("project.json"),
        });
        for legal_name in ["LICENSE", "LICENSE.txt", "NOTICE", "NOTICE.txt"] {
            if config.project_root.join(legal_name).is_file() {
                copies.push(PackageCopy {
                    source: PathBuf::from(legal_name),
                    destination: PathBuf::from(legal_name),
                });
            }
        }
        copies.push(PackageCopy {
            source: PathBuf::from("project_settings.json"),
            destination: PathBuf::from("project_settings.json"),
        });
        let scenes_root = config.project_root.join("assets/scenes");
        if scenes_root.is_dir() {
            for scene in collect_regular_files(&scenes_root) {
                let Ok(relative) = scene.strip_prefix(&config.project_root) else {
                    continue;
                };
                let relative = relative.to_path_buf();
                copies.push(PackageCopy {
                    source: relative.clone(),
                    destination: relative,
                });
            }
        } else if let Some(start_scene) = &config.start_scene {
            // Keep the configured scene in the plan even when the directory is
            // absent. Package execution will then report the exact missing file
            // instead of emitting a superficially successful, unbootable build.
            let relative = PathBuf::from("assets").join(start_scene);
            copies.push(PackageCopy {
                source: relative.clone(),
                destination: relative,
            });
        }
        let mut asset_files = BTreeSet::new();
        for entry in manifest.iter().map(|(_, entry)| entry) {
            asset_files.insert(PathBuf::from(&entry.path));
            asset_files.extend(
                entry
                    .import_settings
                    .source_dependencies
                    .iter()
                    .map(PathBuf::from),
            );
        }
        for asset_file in asset_files {
            let relative = PathBuf::from("assets").join(asset_file);
            copies.push(PackageCopy {
                source: relative.clone(),
                destination: relative,
            });
        }

        let (retarget_copies, retarget_diagnostics) =
            bake_registered_retarget_clips(config, manifest);
        diagnostics.extend(retarget_diagnostics);
        let (humanoid_copies, humanoid_diagnostics) =
            bake_registered_humanoid_clips(config, manifest);
        diagnostics.extend(humanoid_diagnostics);
        if diagnostics.iter().any(|diagnostic| diagnostic.blocking) {
            copies.clear();
        } else {
            copies.extend(retarget_copies);
            copies.extend(humanoid_copies);
        }
    }

    let success = !diagnostics.iter().any(|diagnostic| diagnostic.blocking);

    PackagePlan {
        copies,
        diagnostics,
        success,
    }
}

/// Bakes the registered `*.retarget.json` maps that scene/prefab content
/// actually needs (ADR 0079 §4, narrowed by AP-7) and stages the baked clips
/// as package copies under a deterministic `baked_anim/` path.
///
/// Scope note (AP-7 reachability trace): a *needed pair* is a
/// `(source_skeleton, target_skeleton)` combination read off an entity that
/// carries both `engine.animation_controller` and the `engine.skinned_model`
/// owning its rig, in a `.scene.json` or
/// `.prefab.json` document [`collect_needed_retarget_pairs`] scans (prefabs
/// are roots unconditionally — they may be spawned by script at runtime, so
/// their reachability cannot be ruled out statically, consistent with
/// [`analyze_build`]'s own documented conservative policy). The bake set is
/// every registered map matching a needed pair, unioned with every map that
/// sets [`engine::RetargetMap::always_package`] (the escape hatch for clips
/// assigned dynamically at runtime, invisible to this static walk). A needed
/// pair with no registered map is unchanged from before AP-7: it is not
/// diagnosed here, since editor Play and the player's own animator
/// resolution already surface `anim.retarget_map_missing` for it. A
/// registered map that matches no needed pair and does not set
/// `always_package` is skipped with a non-blocking
/// [`BuildDiagnosticKind::RetargetMapNotReached`] diagnostic rather than
/// baked, so the narrowing is observable in the build report. An unresolvable
/// skeleton or a bake failure for a map that *is* in the bake set is still a
/// blocking diagnostic, consistent with the [`BuildDiagnosticKind::MissingAsset`]
/// policy (ADR 0045): packaging refuses to ship a hole rather than silently
/// drop a retarget it committed to baking.
fn bake_registered_retarget_clips(
    config: &BuildConfig,
    manifest: &AssetManifest,
) -> (Vec<PackageCopy>, Vec<BuildDiagnostic>) {
    let assets_root = config.project_root.join("assets");
    let maps = engine::load_registered_retarget_maps(&assets_root, manifest);
    if maps.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let mut copies = Vec::new();
    let mut diagnostics = Vec::new();
    let mut imports: BTreeMap<AssetId, engine::GltfImportResult> = BTreeMap::new();
    let cache = engine::DerivedCache::new(&config.project_root);
    let needed_pairs = collect_needed_retarget_pairs(manifest, &assets_root, &mut imports);

    for (map_id, map) in &maps {
        let map_path = manifest
            .get(map_id)
            .map(|entry| entry.path.clone())
            .unwrap_or_else(|| map_id.as_str().to_owned());

        if !map.always_package
            && !needed_pairs.contains(&(map.source_skeleton.clone(), map.target_skeleton.clone()))
        {
            diagnostics.push(BuildDiagnostic::non_blocking(
                BuildDiagnosticKind::RetargetMapNotReached {
                    path: map_path.clone(),
                },
                format!(
                    "retarget map '{map_path}' is registered but no scanned scene or prefab needs its \
                     (source, target) skeleton pair; skipping its bake (set always_package to force it)"
                ),
            ));
            continue;
        }

        let source_source_id =
            locate_skeleton_source_id(&map.source_skeleton, manifest, &assets_root, &mut imports);
        let target_source_id =
            locate_skeleton_source_id(&map.target_skeleton, manifest, &assets_root, &mut imports);
        let (Some(source_source_id), Some(target_source_id)) = (source_source_id, target_source_id)
        else {
            diagnostics.push(BuildDiagnostic::blocking(
                BuildDiagnosticKind::RetargetSkeletonUnresolved {
                    path: map_path.clone(),
                },
                format!(
                    "retarget map '{map_path}' references a skeleton not found among the project's model sources"
                ),
            ));
            continue;
        };
        let source_skeleton = imports
            .get(&source_source_id)
            .and_then(|imported| {
                imported
                    .skins
                    .iter()
                    .find(|skin| skin.skeleton.id == map.source_skeleton)
            })
            .map(|skin| skin.skeleton.clone())
            .expect("locate_skeleton_source_id only returns a source that matched");
        let target_skeleton = imports
            .get(&target_source_id)
            .and_then(|imported| {
                imported
                    .skins
                    .iter()
                    .find(|skin| skin.skeleton.id == map.target_skeleton)
            })
            .map(|skin| skin.skeleton.clone())
            .expect("locate_skeleton_source_id only returns a source that matched");
        let source_import = imports
            .get(&source_source_id)
            .expect("looked up by its own key");
        // Baked contact intervals are re-detected against the *target*
        // skeleton (ADR 0080 §1), so the override that can change the baked
        // output is the target source's own `contact_bones`, not the source
        // clip's.
        let target_contact_bones = manifest
            .get(&target_source_id)
            .map(|entry| entry.import_settings.contact_bones.clone())
            .unwrap_or_default();

        for animation in &source_import.animations {
            if source_import
                .skins
                .get(animation.skin_index)
                .is_none_or(|skin| skin.skeleton.id != source_skeleton.id)
            {
                continue;
            }
            let fingerprint = manifest
                .iter()
                .find(|(_, entry)| {
                    entry
                        .import_settings
                        .sub_assets
                        .iter()
                        .any(|sub_asset| sub_asset.id == animation.id.as_str())
                })
                .and_then(|(_, entry)| entry.import_settings.source_fingerprint.clone());
            // A missing fingerprint must block the bake rather than fall back
            // to the clip sub-asset ID: that fallback is only unique within
            // this process and would poison the on-disk derived-clip cache
            // across builds (AP-6; ADR 0079 §3 cache key requires the source
            // fingerprint).
            let Some(fingerprint) = fingerprint else {
                diagnostics.push(BuildDiagnostic::blocking(
                    BuildDiagnosticKind::RetargetSourceUnfingerprinted {
                        path: map_path.clone(),
                        clip: animation.id.as_str().to_owned(),
                    },
                    format!(
                        "retarget map '{map_path}' bakes clip '{}' whose source has no recorded fingerprint; reimport the source to record one before packaging",
                        animation.id.as_str()
                    ),
                ));
                continue;
            };

            let key = match engine::cache_key_for_retargeted_clip(
                &fingerprint,
                animation.id.as_str(),
                source_skeleton.identity,
                target_skeleton.identity,
                map,
                &target_contact_bones,
            ) {
                Ok(key) => key,
                Err(error) => {
                    diagnostics.push(BuildDiagnostic::blocking(
                        BuildDiagnosticKind::RetargetBakeFailed {
                            path: map_path.clone(),
                            reason: error.to_string(),
                        },
                        format!(
                            "failed to compute the retarget cache key for '{map_path}': {error}"
                        ),
                    ));
                    continue;
                }
            };
            if let Err(error) = engine::resolve_or_bake_retargeted_clip(
                &cache,
                &animation.clip,
                &fingerprint,
                animation.id.as_str(),
                &source_skeleton,
                &target_skeleton,
                map,
                &target_contact_bones,
            ) {
                diagnostics.push(BuildDiagnostic::blocking(
                    BuildDiagnosticKind::RetargetBakeFailed {
                        path: map_path.clone(),
                        reason: error.to_string(),
                    },
                    format!("failed to bake a retargeted clip for '{map_path}': {error}"),
                ));
                continue;
            }

            let file_name = format!("{}.{}", key.file_stem(), engine::BAKED_CLIP_FILE_EXTENSION);
            copies.push(PackageCopy {
                source: PathBuf::from(".engine/cache")
                    .join(engine::RETARGET_CACHE_DOMAIN)
                    .join(&file_name),
                destination: PathBuf::from("baked_anim").join(&file_name),
            });
        }
    }

    (copies, diagnostics)
}

/// Pre-bakes every registered HumanoidMotion against every usable persisted
/// HumanoidProfile and stages the resulting target-bound clips for packaged
/// playback (ADR 0110).
///
/// This intentionally follows the packager's conservative v1 reachability
/// policy instead of trying to infer every possible runtime `Auto`/`Humanoid`
/// choice. The shipped player never imports a model or solves a Humanoid bake:
/// it resolves the deterministic `(motion stable ID, target skeleton stable ID)`
/// package path written here.
fn bake_registered_humanoid_clips(
    config: &BuildConfig,
    manifest: &AssetManifest,
) -> (Vec<PackageCopy>, Vec<BuildDiagnostic>) {
    let assets_root = config.project_root.join("assets");
    let cache = engine::DerivedCache::new(&config.project_root);
    let mut imports: BTreeMap<AssetId, engine::GltfImportResult> = BTreeMap::new();
    let mut diagnostics = Vec::new();

    let mut first_profiles = BTreeMap::<AssetId, engine::asset::HumanoidProfile>::new();
    let mut motion_sources = Vec::<(AssetId, Vec<AssetId>)>::new();
    for (source_id, entry) in manifest.iter() {
        for profile in &entry.import_settings.humanoid_profiles {
            let Ok(skeleton_id) =
                AssetId::from_stable_id(StableId::new(&profile.skeleton))
            else {
                continue;
            };
            first_profiles
                .entry(skeleton_id)
                .or_insert_with(|| profile.clone());
        }

        let motion_ids = entry
            .import_settings
            .sub_assets
            .iter()
            .filter(|sub_asset| {
                sub_asset.kind == engine::ImportedSubAssetKind::HumanoidMotion
            })
            .filter_map(|sub_asset| {
                AssetId::from_stable_id(StableId::new(&sub_asset.id)).ok()
            })
            .collect::<Vec<_>>();
        if !motion_ids.is_empty() {
            motion_sources.push((source_id.clone(), motion_ids));
        }
    }

    if motion_sources.is_empty() || first_profiles.is_empty() {
        return (Vec::new(), diagnostics);
    }

    let mut targets = BTreeMap::<
        AssetId,
        (
            engine::skeleton_asset::SkeletonAsset,
            engine::asset::HumanoidProfile,
        ),
    >::new();
    for (target_id, profile) in first_profiles {
        let Some(target_source_id) =
            locate_skeleton_source_id(&target_id, manifest, &assets_root, &mut imports)
        else {
            continue;
        };
        let Some(target) = imports
            .get(&target_source_id)
            .and_then(|imported| {
                imported
                    .skins
                    .iter()
                    .find(|skin| skin.skeleton.id == target_id)
            })
            .map(|skin| skin.skeleton.clone())
        else {
            continue;
        };
        if engine::humanoid::validate_humanoid_profile(&profile, &target).is_ok() {
            targets.insert(target_id, (target, profile));
        }
    }

    if targets.is_empty() {
        return (Vec::new(), diagnostics);
    }

    let mut motions = BTreeMap::<AssetId, engine::humanoid_motion::HumanoidMotion>::new();
    for (source_id, registered_motion_ids) in motion_sources {
        let Some(entry) = manifest.get(&source_id) else {
            continue;
        };
        let existing_profiles = entry.import_settings.humanoid_profiles.clone();
        let source_path = entry.path.clone();
        let Some(imported) =
            import_source_for_reachability(&source_id, manifest, &assets_root, &mut imports)
        else {
            for motion_id in registered_motion_ids {
                diagnostics.push(BuildDiagnostic::blocking(
                    BuildDiagnosticKind::HumanoidBakeFailed {
                        motion: motion_id.as_str().to_owned(),
                        target_skeleton: None,
                        reason: "owning model source could not be imported".to_owned(),
                    },
                    format!(
                        "Humanoid motion '{}' is registered by source '{}' but that model could not be imported during packaging",
                        motion_id.as_str(),
                        source_path
                    ),
                ));
            }
            continue;
        };

        let catalog =
            engine::humanoid_import::build_humanoid_import_catalog(imported, &existing_profiles);
        for motion_id in registered_motion_ids {
            let Some(portable) = catalog
                .motions
                .iter()
                .find(|motion| motion.id == motion_id)
            else {
                diagnostics.push(BuildDiagnostic::blocking(
                    BuildDiagnosticKind::HumanoidBakeFailed {
                        motion: motion_id.as_str().to_owned(),
                        target_skeleton: None,
                        reason: "registered HumanoidMotion is no longer produced by import".to_owned(),
                    },
                    format!(
                        "Humanoid motion '{}' is registered by source '{}' but reimport no longer produces it; reimport the source before packaging",
                        motion_id.as_str(),
                        source_path
                    ),
                ));
                continue;
            };
            motions.insert(motion_id, portable.motion.clone());
        }
    }

    let mut copies = Vec::new();
    for (motion_id, motion) in &motions {
        for (target_id, (target, profile)) in &targets {
            let key = match engine::humanoid_motion::humanoid_bake_cache_key(
                motion, target, profile,
            ) {
                Ok(key) => key,
                Err(error) => {
                    diagnostics.push(BuildDiagnostic::blocking(
                        BuildDiagnosticKind::HumanoidBakeFailed {
                            motion: motion_id.as_str().to_owned(),
                            target_skeleton: Some(target_id.as_str().to_owned()),
                            reason: error.to_string(),
                        },
                        format!(
                            "failed to compute Humanoid bake key for motion '{}' and target skeleton '{}': {error}",
                            motion_id.as_str(),
                            target_id.as_str()
                        ),
                    ));
                    continue;
                }
            };
            if let Err(error) = engine::humanoid_motion::resolve_or_bake_humanoid_motion(
                &cache, motion, target, profile,
            ) {
                diagnostics.push(BuildDiagnostic::blocking(
                    BuildDiagnosticKind::HumanoidBakeFailed {
                        motion: motion_id.as_str().to_owned(),
                        target_skeleton: Some(target_id.as_str().to_owned()),
                        reason: error.to_string(),
                    },
                    format!(
                        "failed to bake Humanoid motion '{}' for target skeleton '{}': {error}",
                        motion_id.as_str(),
                        target_id.as_str()
                    ),
                ));
                continue;
            }

            let cache_file_name = format!(
                "{}.{}",
                key.file_stem(),
                engine::humanoid_motion::HUMANOID_BAKED_CLIP_FILE_EXTENSION
            );
            let package_file_name =
                engine::humanoid_motion::humanoid_packaged_bake_file_name(
                    motion_id, target_id,
                );
            copies.push(PackageCopy {
                source: PathBuf::from(".engine/cache")
                    .join(engine::humanoid_motion::HUMANOID_CACHE_DOMAIN)
                    .join(cache_file_name),
                destination: PathBuf::from("baked_anim")
                    .join("humanoid")
                    .join(package_file_name),
            });
        }
    }

    (copies, diagnostics)
}
// ---------------------------------------------------------------------------
// AP-7 reachability trace
// ---------------------------------------------------------------------------

/// Walks every `.scene.json` / `.prefab.json` document reachable from
/// `manifest` and `assets_root` and returns the set of
/// `(source_skeleton, target_skeleton)` pairs a unified animation controller
/// or legacy animator/skeleton entity actually needs a retarget for (AP-7).
///
/// Prefabs are registered manifest assets, so their entries are found by
/// scanning `manifest` directly. Scenes are not manifest assets in this
/// project (see [`plan_package`]'s own scene-copy step, which walks
/// `assets/scenes` for the same reason), so `assets_root/scenes` is scanned
/// as well via [`collect_regular_files`]. Malformed scene/prefab documents
/// are skipped rather than failing this pass — their own structural
/// diagnostics surface through scene/prefab validation, not here.
///
/// `imports` is shared with the caller's own bake loop so a source imported
/// while resolving reachability is not imported a second time while baking.
fn collect_needed_retarget_pairs(
    manifest: &AssetManifest,
    assets_root: &Path,
    imports: &mut BTreeMap<AssetId, engine::GltfImportResult>,
) -> BTreeSet<(AssetId, AssetId)> {
    let mut document_paths: BTreeSet<PathBuf> = BTreeSet::new();
    for (_, entry) in manifest.iter() {
        let lower = entry.path.to_ascii_lowercase();
        if lower.ends_with(".scene.json") || lower.ends_with(".prefab.json") {
            document_paths.insert(assets_root.join(&entry.path));
        }
    }
    let scenes_root = assets_root.join("scenes");
    if scenes_root.is_dir() {
        for path in collect_regular_files(&scenes_root) {
            if path
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(".scene.json")
            {
                document_paths.insert(path);
            }
        }
    }

    let mut needed = BTreeSet::new();
    for path in document_paths {
        let Ok(json) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lower = path.to_string_lossy().to_ascii_lowercase();
        let entities: Vec<engine_authoring::AuthoringEntity> = if lower.ends_with(".scene.json") {
            match engine_authoring::load_scene_from_json(&json) {
                Ok(scene) => scene.entities().map(|(_, entity)| entity.clone()).collect(),
                Err(_) => continue,
            }
        } else if lower.ends_with(".prefab.json") {
            match engine_authoring::PrefabAsset::from_json(&json) {
                Ok(prefab) => prefab.entities.into_values().collect(),
                Err(_) => continue,
            }
        } else {
            continue;
        };
        for entity in &entities {
            collect_needed_pairs_for_entity(entity, manifest, assets_root, imports, &mut needed);
        }
    }
    needed
}

/// Resolves the needed `(source_skeleton, target_skeleton)` pairs for one
/// entity, contributing them to `needed` when it carries an Animation
/// Controller and the Skinned Model that owns its rig (AP-7, ADR 0087).
fn collect_needed_pairs_for_entity(
    entity: &engine_authoring::AuthoringEntity,
    manifest: &AssetManifest,
    assets_root: &Path,
    imports: &mut BTreeMap<AssetId, engine::GltfImportResult>,
    needed: &mut BTreeSet<(AssetId, AssetId)>,
) {
    let Some(controller) = entity
        .components
        .get(&engine_authoring::id::ComponentTypeId::new(
            engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT,
        ))
    else {
        return;
    };
    let Some(model) = entity
        .components
        .get(&engine_authoring::id::ComponentTypeId::new(
            engine::scene_bridge::SKINNED_MODEL_COMPONENT,
        ))
    else {
        return;
    };
    if matches!(
        controller,
        engine_authoring::Value::Object(fields)
            if fields.get("enabled") == Some(&engine_authoring::Value::Bool(false))
    ) {
        return;
    }

    let Some(target_skeleton) = resolve_target_skeleton_id(model, manifest, assets_root, imports)
    else {
        return;
    };
    for source_skeleton in resolve_source_skeleton_ids(controller, manifest, assets_root, imports) {
        if source_skeleton != target_skeleton {
            needed.insert((source_skeleton, target_skeleton.clone()));
        }
    }
}

/// Resolves a Skinned Model's `skeleton` field to the canonical (post-dedupe)
/// skeleton `AssetId` its rig is built from (AP-7).
///
/// The owning source is found via
/// [`engine::AssetManifest::imported_sub_asset`], reimported the same way
/// [`locate_skeleton_source_id`] does, and the matching skin's
/// [`engine::skeleton_asset::SkeletonAsset::id`] (accessible here through
/// [`engine::GltfSkinData::skeleton`]) is read directly rather than
/// re-derived, since the ADR 0077 §4 dedupe rule can adopt an ID that differs
/// from the catalog entry the author picked. This mirrors how
/// `spawn_skinned_model_component` reaches the same rig.
fn resolve_target_skeleton_id(
    value: &engine_authoring::Value,
    manifest: &AssetManifest,
    assets_root: &Path,
    imports: &mut BTreeMap<AssetId, engine::GltfImportResult>,
) -> Option<AssetId> {
    let engine_authoring::Value::Object(fields) = value else {
        return None;
    };
    let Some(engine_authoring::Value::AssetRef(skeleton_ref)) = fields.get("skeleton") else {
        return None;
    };
    let (source_id, _entry, sub_asset) = manifest.imported_sub_asset(skeleton_ref)?;
    if sub_asset.kind != engine::ImportedSubAssetKind::Skeleton {
        return None;
    }
    let source_id = source_id.clone();
    let imported = import_source_for_reachability(&source_id, manifest, assets_root, imports)?;
    imported
        .skins
        .iter()
        .find(|skin| &skin.skeleton_id == skeleton_ref)
        .map(|skin| skin.skeleton.id.clone())
}

/// Resolves a controller's Animation Set to every source-skeleton `AssetId`
/// that may need a retarget bake (AP-7, ADR 0085).
fn resolve_source_skeleton_ids(
    value: &engine_authoring::Value,
    manifest: &AssetManifest,
    assets_root: &Path,
    imports: &mut BTreeMap<AssetId, engine::GltfImportResult>,
) -> Vec<AssetId> {
    let engine_authoring::Value::Object(fields) = value else {
        return Vec::new();
    };
    if let Some(engine_authoring::Value::AssetRef(animation_set_id)) = fields.get("animation_set") {
        let Some(entry) = manifest.get(animation_set_id) else {
            return Vec::new();
        };
        let Ok(json) = std::fs::read_to_string(assets_root.join(&entry.path)) else {
            return Vec::new();
        };
        let Ok(animation_set) = engine_authoring::AnimationSet::from_json(&json) else {
            return Vec::new();
        };
        let mut skeletons = BTreeSet::new();
        for binding in animation_set.bindings.values() {
            for clip in std::iter::once(&binding.clip).chain(&binding.overlays) {
                skeletons.extend(resolve_clip_source_skeleton_ids(
                    clip,
                    manifest,
                    assets_root,
                    imports,
                ));
            }
        }
        return skeletons.into_iter().collect();
    }
    Vec::new()
}

/// Resolves one Animation Set clip sub-asset reference to the skeleton
/// identities used by its imported clips.
fn resolve_clip_source_skeleton_ids(
    clip_ref: &AssetId,
    manifest: &AssetManifest,
    assets_root: &Path,
    imports: &mut BTreeMap<AssetId, engine::GltfImportResult>,
) -> Vec<AssetId> {
    if let Some((source_id, _entry, sub_asset)) = manifest.imported_sub_asset(clip_ref) {
        if sub_asset.kind != engine::ImportedSubAssetKind::Animation {
            return Vec::new();
        }
        if manifest.get(source_id).is_some_and(|entry| {
            engine::asset_path_matches_kind(
                engine::AssetKind::MotionSource,
                Path::new(&entry.path),
            )
        }) {
            let target = sub_asset.target_model_source.as_deref().or_else(|| {
                manifest
                    .get(source_id)
                    .and_then(|entry| {
                        entry
                            .import_settings
                            .resolved_motion_model_sources()
                            .first()
                            .copied()
                    })
            });
            let Some(target) = target.and_then(|target| {
                AssetId::from_stable_id(engine_authoring::StableId::new(target)).ok()
            }) else {
                return Vec::new();
            };
            return manifest
                .get(&target)
                .into_iter()
                .flat_map(|entry| &entry.import_settings.skeleton_records)
                .filter_map(|record| {
                    AssetId::from_stable_id(engine_authoring::StableId::new(&record.id)).ok()
                })
                .collect();
        }
        let source_id = source_id.clone();
        let Some(imported) =
            import_source_for_reachability(&source_id, manifest, assets_root, imports)
        else {
            return Vec::new();
        };
        return imported
            .animations
            .iter()
            .find(|animation| &animation.id == clip_ref)
            .and_then(|animation| imported.skins.get(animation.skin_index))
            .map(|skin| vec![skin.skeleton.id.clone()])
            .unwrap_or_default();
    }

    if manifest.get(clip_ref).is_none() {
        return Vec::new();
    }
    let Some(imported) = import_source_for_reachability(clip_ref, manifest, assets_root, imports)
    else {
        return Vec::new();
    };
    imported
        .skins
        .iter()
        .map(|skin| skin.skeleton.id.clone())
        .collect()
}

/// Imports `source_id` on demand (caching into `imports`), mirroring
/// [`locate_skeleton_source_id`]'s own reimport call: the manifest's
/// persisted `skeleton_records` ledger is passed through so dedupe adopts the
/// same canonical skeleton IDs already recorded on disk instead of minting
/// fresh ones.
fn import_source_for_reachability<'a>(
    source_id: &AssetId,
    manifest: &AssetManifest,
    assets_root: &Path,
    imports: &'a mut BTreeMap<AssetId, engine::GltfImportResult>,
) -> Option<&'a engine::GltfImportResult> {
    if !imports.contains_key(source_id) {
        let entry = manifest.get(source_id)?;
        let path = assets_root.join(&entry.path);
        if !engine::asset_path_matches_kind(engine::AssetKind::GltfSource, &path) {
            return None;
        }
        let existing_skeletons = entry.import_settings.skeleton_records.clone();
        let imported = engine::import_model_path(source_id, &path, &existing_skeletons).ok()?;
        imports.insert(source_id.clone(), imported);
    }
    imports.get(source_id)
}

/// Finds the model source [`engine_authoring::id::AssetId`] (glTF/GLB or FBX,
/// ADR 0081) whose imported skins adopted `wanted` as a skeleton ID,
/// importing (and caching into `imports`) manifest model sources on demand
/// until a match is found or every source has been tried.
///
/// Returns an owned ID rather than a borrow of `imports` so a second,
/// independent call (resolving the map's other skeleton) can still mutate
/// `imports` afterward.
fn locate_skeleton_source_id(
    wanted: &AssetId,
    manifest: &AssetManifest,
    assets_root: &Path,
    imports: &mut BTreeMap<AssetId, engine::GltfImportResult>,
) -> Option<AssetId> {
    if let Some((source_id, _)) = imports.iter().find(|(_, imported)| {
        imported
            .skins
            .iter()
            .any(|skin| &skin.skeleton.id == wanted)
    }) {
        return Some(source_id.clone());
    }

    for (source_id, entry) in manifest.iter() {
        if imports.contains_key(source_id) {
            continue;
        }
        let path = assets_root.join(&entry.path);
        if !engine::asset_path_matches_kind(engine::AssetKind::GltfSource, &path) {
            continue;
        }
        let existing_skeletons = entry.import_settings.skeleton_records.clone();
        let Ok(imported) = engine::import_model_path(source_id, &path, &existing_skeletons) else {
            continue;
        };
        let matched = imported
            .skins
            .iter()
            .any(|skin| &skin.skeleton.id == wanted);
        let id = source_id.clone();
        imports.insert(id.clone(), imported);
        if matched {
            return Some(id);
        }
    }
    None
}

/// Returns all regular files below `root` in deterministic path order.
///
/// Scene documents may be organized into subdirectories, so package planning
/// must recurse instead of assuming a flat `scenes/` folder. Entries that
/// cannot be inspected are skipped here and will still surface as explicit I/O
/// failures if they are required by another package copy contract.
fn collect_regular_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => pending.push(path),
                Ok(file_type) if file_type.is_file() => files.push(path),
                _ => {}
            }
        }
    }
    files.sort();
    files
}

fn is_safe_asset_relative_path(path: &str) -> bool {
    let path = std::path::Path::new(path);
    !path.is_absolute()
        && !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

/// Errors that can occur while executing a package plan.
#[derive(Debug)]
pub enum PackageError {
    /// Planning found blocking diagnostics.
    PlanFailed(Vec<BuildDiagnostic>),
    /// A filesystem operation failed.
    Io {
        /// What the packager was doing when the error occurred.
        context: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The asset manifest could not be serialized.
    ManifestSerialize(serde_json::Error),
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PlanFailed(_) => write!(f, "package planning produced blocking diagnostics"),
            Self::Io { context, source } => write!(f, "{context}: {source}"),
            Self::ManifestSerialize(error) => {
                write!(f, "failed to serialize asset manifest: {error}")
            }
        }
    }
}

impl std::error::Error for PackageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PlanFailed(_) => None,
            Self::Io { source, .. } => Some(source),
            Self::ManifestSerialize(error) => Some(error),
        }
    }
}

/// Executes a package plan into `config.output_dir` (ADR 0045).
///
/// `player_binary` is the prebuilt player executable
/// (`target/<profile>/player[.exe]` in the source workspace); it is copied
/// to `game.exe` (`game` on non-Windows). Building the player binary is the
/// caller's responsibility, which keeps this function free of subprocesses.
///
/// # Errors
///
/// Returns [`PackageError::PlanFailed`] when planning has blocking
/// diagnostics, and I/O or serialization errors when the copy fails.
pub fn package_project(
    config: &BuildConfig,
    manifest: &AssetManifest,
    player_binary: &std::path::Path,
) -> Result<PackagePlan, PackageError> {
    package_project_with_game_module(config, manifest, player_binary, None)
}

/// Executes a package plan and optionally includes project Rust game code.
pub fn package_project_with_game_module(
    config: &BuildConfig,
    manifest: &AssetManifest,
    player_binary: &std::path::Path,
    game_module: Option<&std::path::Path>,
) -> Result<PackagePlan, PackageError> {
    let plan = plan_package(config, manifest);
    if !plan.success {
        let blocking: Vec<BuildDiagnostic> = plan
            .diagnostics
            .into_iter()
            .filter(|diagnostic| diagnostic.blocking)
            .collect();
        return Err(PackageError::PlanFailed(blocking));
    }

    std::fs::create_dir_all(&config.output_dir).map_err(|source| PackageError::Io {
        context: format!("failed to create {}", config.output_dir.display()),
        source,
    })?;

    let executable_name = if cfg!(windows) { "game.exe" } else { "game" };
    let executable_destination = config.output_dir.join(executable_name);
    std::fs::copy(player_binary, &executable_destination).map_err(|source| PackageError::Io {
        context: format!(
            "failed to copy player binary {} to {}",
            player_binary.display(),
            executable_destination.display()
        ),
        source,
    })?;

    if let Some(game_module) = game_module {
        let destination = config
            .output_dir
            .join(engine::game_module::packaged_game_module_file_name());
        std::fs::copy(game_module, &destination).map_err(|source| PackageError::Io {
            context: format!(
                "failed to copy game module {} to {}",
                game_module.display(),
                destination.display()
            ),
            source,
        })?;
    }

    for copy in &plan.copies {
        let source_path = config.project_root.join(&copy.source);
        let destination_path = config.output_dir.join(&copy.destination);
        if let Some(parent) = destination_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| PackageError::Io {
                context: format!("failed to create {}", parent.display()),
                source,
            })?;
        }
        std::fs::copy(&source_path, &destination_path).map_err(|source| PackageError::Io {
            context: format!(
                "failed to copy {} to {}",
                source_path.display(),
                destination_path.display()
            ),
            source,
        })?;
    }

    // The manifest is a generated build artifact here, not the project's
    // source of truth, so a plain write is acceptable (the editor-side
    // manifest saves keep using replace_file_contents).
    let manifest_json = manifest
        .to_canonical_json()
        .map_err(PackageError::ManifestSerialize)?;
    let manifest_destination = config.output_dir.join("asset_manifest.json");
    std::fs::write(&manifest_destination, manifest_json).map_err(|source| PackageError::Io {
        context: format!("failed to write {}", manifest_destination.display()),
        source,
    })?;

    let notices_destination = config.output_dir.join("THIRD_PARTY_NOTICES.txt");
    std::fs::write(
        &notices_destination,
        "Rust Game Engine package\n\nDependency license texts are distributed with the engine release.\nProject and asset notices copied into this directory remain authoritative.\n",
    )
    .map_err(|source| PackageError::Io {
        context: format!("failed to write {}", notices_destination.display()),
        source,
    })?;

    let mut packaged_files = plan
        .copies
        .iter()
        .map(|copy| copy.destination.to_string_lossy().replace('\\', "/"))
        .collect::<Vec<_>>();
    packaged_files.extend([
        executable_name.to_string(),
        "asset_manifest.json".to_string(),
        "THIRD_PARTY_NOTICES.txt".to_string(),
    ]);
    if game_module.is_some() {
        packaged_files.push(engine::game_module::packaged_game_module_file_name().to_string());
    }
    packaged_files.sort();
    packaged_files.dedup();
    let report = serde_json::json!({
        "schema_version": 1,
        "success": true,
        "start_scene": config.start_scene,
        "files": packaged_files,
        "game_module_included": game_module.is_some(),
        "save_location": "os_local_data",
        "portable_save_opt_in": "GAMEENGINE_PORTABLE_SAVES",
        "debug_symbols": "not bundled; retain build artifacts for symbolication",
        "crash_policy": "startup failures are appended to the OS local-data log directory"
    });
    let report_json =
        serde_json::to_string_pretty(&report).map_err(PackageError::ManifestSerialize)?;
    let report_destination = config.output_dir.join("build_report.json");
    std::fs::write(&report_destination, report_json).map_err(|source| PackageError::Io {
        context: format!("failed to write {}", report_destination.display()),
        source,
    })?;

    Ok(plan)
}

/// Locates a prebuilt player binary for the editor Package action.
///
/// Installed builds prefer a `player` executable next to the editor. Local
/// workspace builds fall back to `target/release/player`. The function never
/// launches Cargo; build the binary with
/// `cargo build -p engine --release --bin player` when no candidate exists.
pub fn find_player_binary() -> Option<PathBuf> {
    let executable_name = if cfg!(windows) {
        "player.exe"
    } else {
        "player"
    };
    let mut candidates = Vec::new();
    if let Ok(editor_executable) = std::env::current_exe()
        && let Some(directory) = editor_executable.parent() {
            candidates.push(directory.join(executable_name));
        }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("target/release")
            .join(executable_name),
    );
    candidates.into_iter().find(|candidate| candidate.is_file())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{
        AssetManifest, ImportSettings, ImportedSubAsset, ImportedSubAssetKind, ManifestEntry,
    };
    use engine_authoring::id::AssetId;
    use engine_authoring::{AnimationGraphDomain, Edge, GraphDomain, Node, PortRef};
    use std::path::PathBuf;

    fn config_no_scene() -> BuildConfig {
        BuildConfig {
            project_root: PathBuf::from("/tmp/project"),
            output_dir: PathBuf::from("/tmp/output"),
            start_scene: None,
        }
    }

    fn config_with_scene() -> BuildConfig {
        BuildConfig {
            project_root: PathBuf::from("/tmp/project"),
            output_dir: PathBuf::from("/tmp/output"),
            start_scene: Some("scenes/main.scene.json".into()),
        }
    }

    fn manifest_with(path: &str) -> AssetManifest {
        let mut m = AssetManifest::default();
        m.insert(
            AssetId::generate(),
            ManifestEntry {
                path: path.into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );
        m
    }

    #[test]
    fn no_start_scene_produces_blocking_diagnostic() {
        let report = analyze_build(&config_no_scene(), &AssetManifest::default());
        assert!(!report.success, "missing start scene must block the build");
        assert!(report
            .diagnostics
            .iter()
            .any(|d| d.kind == BuildDiagnosticKind::MissingStartScene));
    }

    #[test]
    fn empty_manifest_with_scene_succeeds() {
        let report = analyze_build(&config_with_scene(), &AssetManifest::default());
        assert!(
            report.success,
            "empty manifest with a start scene must succeed"
        );
        assert!(report.diagnostics.is_empty());
        assert!(report.reachable_assets.is_empty());
    }

    #[test]
    fn all_manifest_entries_included_in_reachable() {
        let manifest = manifest_with("meshes/cube.obj");
        let report = analyze_build(&config_with_scene(), &manifest);
        assert_eq!(
            report.reachable_assets.len(),
            1,
            "all manifest entries must be reachable in v1"
        );
    }

    #[test]
    fn imported_sub_assets_are_included_in_reachability() {
        let source = AssetId::generate();
        let mesh = AssetId::derive(&source, "mesh:0");
        let mut manifest = AssetManifest::default();
        manifest.insert(
            source,
            ManifestEntry {
                path: "characters/hero.glb".into(),
                name: None,
                import_settings: ImportSettings {
                    sub_assets: vec![ImportedSubAsset {
                        id: mesh.as_str().to_owned(),
                        kind: ImportedSubAssetKind::Mesh,
                        name: "Body".into(),
                        index: 0,
                        target_model_source: None,
                    }],
                    ..ImportSettings::default()
                },
            },
        );

        let report = analyze_build(&config_with_scene(), &manifest);
        assert!(report.reachable_assets.contains(&mesh));
        assert_eq!(report.reachable_assets.len(), 2);
    }

    #[test]
    fn humanoid_motion_sub_assets_use_the_manifest_stable_id_contract() {
        let source = AssetId::generate();
        let native =
            engine::imported_sub_asset_id(&source, ImportedSubAssetKind::Animation, 0);
        let humanoid = engine::asset::imported_humanoid_motion_sub_asset_id(&native);
        let mut manifest = AssetManifest::default();
        manifest.insert(
            source,
            ManifestEntry {
                path: "characters/hero.glb".into(),
                name: None,
                import_settings: ImportSettings {
                    sub_assets: vec![ImportedSubAsset {
                        id: humanoid.as_str().to_owned(),
                        kind: ImportedSubAssetKind::HumanoidMotion,
                        name: "Walk (Humanoid)".into(),
                        index: 0,
                        target_model_source: None,
                    }],
                    ..ImportSettings::default()
                },
            },
        );

        let report = analyze_build(&config_with_scene(), &manifest);

        assert!(report.reachable_assets.contains(&humanoid));
        assert!(!report.diagnostics.iter().any(|diagnostic| matches!(
            diagnostic.kind,
            BuildDiagnosticKind::InvalidImportedAssetId { .. }
        )));
    }

    #[test]
    fn package_plan_includes_external_gltf_sidecars_once() {
        let directory = tempfile::tempdir().expect("temp dir");
        let project_root = directory.path().join("project");
        let assets = project_root.join("assets/characters");
        std::fs::create_dir_all(&assets).expect("assets fixture");
        std::fs::write(assets.join("hero.gltf"), b"{}").expect("source fixture");
        std::fs::write(assets.join("hero.bin"), b"buffer").expect("buffer fixture");
        std::fs::write(assets.join("hero.png"), b"texture").expect("texture fixture");
        let mut manifest = AssetManifest::default();
        manifest.insert(
            AssetId::generate(),
            ManifestEntry {
                path: "characters/hero.gltf".into(),
                name: None,
                import_settings: ImportSettings {
                    source_dependencies: vec![
                        "characters/hero.bin".into(),
                        "characters/hero.png".into(),
                        "characters/hero.bin".into(),
                    ],
                    ..ImportSettings::default()
                },
            },
        );
        let config = BuildConfig {
            project_root,
            output_dir: directory.path().join("out"),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = plan_package(&config, &manifest);

        assert!(plan.success);
        let asset_copies = plan
            .copies
            .iter()
            .filter(|copy| copy.source.starts_with("assets/characters"))
            .map(|copy| copy.source.clone())
            .collect::<Vec<_>>();
        assert_eq!(asset_copies.len(), 3);
        assert!(asset_copies.contains(&PathBuf::from("assets/characters/hero.gltf")));
        assert!(asset_copies.contains(&PathBuf::from("assets/characters/hero.bin")));
        assert!(asset_copies.contains(&PathBuf::from("assets/characters/hero.png")));
    }

    #[test]
    fn changed_import_source_blocks_package_until_reimport() {
        let directory = tempfile::tempdir().expect("temp dir");
        let project_root = directory.path().join("project");
        let source = project_root.join("assets/hero.glb");
        std::fs::create_dir_all(source.parent().expect("source parent")).expect("assets fixture");
        std::fs::write(&source, b"first").expect("source fixture");
        let fingerprint =
            engine::fingerprint_gltf_source(&source, &[]).expect("initial fingerprint");
        let mut manifest = AssetManifest::default();
        manifest.insert(
            AssetId::generate(),
            ManifestEntry {
                path: "hero.glb".into(),
                name: None,
                import_settings: ImportSettings {
                    source_fingerprint: Some(fingerprint),
                    ..ImportSettings::default()
                },
            },
        );
        std::fs::write(&source, b"second").expect("modified source fixture");
        let config = BuildConfig {
            project_root,
            output_dir: directory.path().join("out"),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = plan_package(&config, &manifest);

        assert!(!plan.success);
        assert!(plan.diagnostics.iter().any(|diagnostic| matches!(
            diagnostic.kind,
            BuildDiagnosticKind::StaleImportedSource { .. }
        )));
    }

    #[test]
    fn plan_package_escalates_missing_assets_to_blocking() {
        let manifest = manifest_with("meshes/does_not_exist.obj");
        let plan = plan_package(&config_with_scene(), &manifest);

        assert!(!plan.success, "missing asset must block packaging");
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.blocking));
        assert!(plan.copies.is_empty(), "failed plans must not list copies");
    }

    #[test]
    fn package_project_produces_runnable_layout() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let project_root = dir.path().join("project");
        let output_dir = dir.path().join("out");
        std::fs::create_dir_all(project_root.join("assets").join("meshes"))
            .expect("project dirs must be created");
        std::fs::create_dir_all(project_root.join("assets/scenes"))
            .expect("scene fixture directory must be created");
        std::fs::write(project_root.join("project.json"), "{}")
            .expect("project.json fixture must write");
        std::fs::write(project_root.join("project_settings.json"), "{}")
            .expect("settings fixture must write");
        std::fs::write(project_root.join("assets/scenes/main.scene.json"), "{}")
            .expect("scene fixture must write");
        std::fs::write(
            project_root.join("assets").join("meshes").join("tri.obj"),
            "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n",
        )
        .expect("asset fixture must write");
        let player_binary = dir.path().join("player.exe");
        std::fs::write(&player_binary, b"player-binary-placeholder")
            .expect("player fixture must write");

        let manifest = manifest_with("meshes/tri.obj");
        let config = BuildConfig {
            project_root: project_root.clone(),
            output_dir: output_dir.clone(),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = package_project(&config, &manifest, &player_binary)
            .expect("packaging a complete project must succeed");

        assert!(plan.success);
        let executable = if cfg!(windows) { "game.exe" } else { "game" };
        assert!(output_dir.join(executable).exists());
        assert!(output_dir.join("project.json").exists());
        assert!(output_dir.join("project_settings.json").exists());
        assert!(output_dir.join("assets/scenes/main.scene.json").exists());
        assert!(output_dir.join("asset_manifest.json").exists());
        assert!(output_dir.join("build_report.json").exists());
        assert!(output_dir.join("THIRD_PARTY_NOTICES.txt").exists());
        assert!(output_dir
            .join("assets")
            .join("meshes")
            .join("tri.obj")
            .exists());
    }

    #[test]
    fn package_handles_spaces_and_japanese_paths() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let project_root = dir.path().join("日本語 Project");
        let output_dir = dir.path().join("配布 Output");
        std::fs::create_dir_all(project_root.join("assets/音声 files"))
            .expect("Unicode project dirs must be created");
        std::fs::create_dir_all(project_root.join("assets/scenes"))
            .expect("Unicode scene directory must be created");
        std::fs::write(project_root.join("project.json"), "{}")
            .expect("project fixture must write");
        std::fs::write(project_root.join("project_settings.json"), "{}")
            .expect("settings fixture must write");
        std::fs::write(project_root.join("assets/scenes/タイトル.scene.json"), "{}")
            .expect("Unicode scene fixture must write");
        std::fs::write(project_root.join("assets/音声 files/click.wav"), b"wav")
            .expect("asset fixture must write");
        let player_binary = dir.path().join("player binary.exe");
        std::fs::write(&player_binary, b"player").expect("player fixture must write");
        let manifest = manifest_with("音声 files/click.wav");
        let config = BuildConfig {
            project_root,
            output_dir: output_dir.clone(),
            start_scene: Some("scenes/タイトル.scene.json".into()),
        };

        package_project(&config, &manifest, &player_binary)
            .expect("Unicode and spaces must package");

        assert!(output_dir.join("assets/音声 files/click.wav").is_file());
        assert!(output_dir.join("build_report.json").is_file());
    }

    #[test]
    fn package_project_copies_game_module_under_player_contract_name() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let project_root = dir.path().join("project");
        let output_dir = dir.path().join("out");
        std::fs::create_dir_all(project_root.join("assets")).expect("project dirs must be created");
        std::fs::create_dir_all(project_root.join("assets/scenes"))
            .expect("scene fixture directory must be created");
        std::fs::write(project_root.join("project.json"), "{}")
            .expect("project fixture must write");
        std::fs::write(project_root.join("project_settings.json"), "{}")
            .expect("settings fixture must write");
        std::fs::write(project_root.join("assets/scenes/main.scene.json"), "{}")
            .expect("scene fixture must write");
        let player_binary = dir.path().join("player.exe");
        std::fs::write(&player_binary, b"player").expect("player fixture must write");
        let game_module = dir.path().join("built-module");
        std::fs::write(&game_module, b"module").expect("module fixture must write");
        let config = BuildConfig {
            project_root,
            output_dir: output_dir.clone(),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        package_project_with_game_module(
            &config,
            &AssetManifest::default(),
            &player_binary,
            Some(&game_module),
        )
        .expect("packaging with a game module must succeed");

        assert!(output_dir
            .join(engine::game_module::packaged_game_module_file_name())
            .is_file());
    }

    #[test]
    fn package_project_refuses_missing_asset_files() {
        let dir = tempfile::tempdir().expect("temp dir must be created");
        let project_root = dir.path().join("project");
        let output_dir = dir.path().join("out");
        std::fs::create_dir_all(project_root.join("assets")).expect("project dirs must be created");
        let player_binary = dir.path().join("player.exe");
        std::fs::write(&player_binary, b"player-binary-placeholder")
            .expect("player fixture must write");

        let manifest = manifest_with("meshes/never_written.obj");
        let config = BuildConfig {
            project_root,
            output_dir: output_dir.clone(),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let result = package_project(&config, &manifest, &player_binary);

        assert!(
            matches!(result, Err(PackageError::PlanFailed(_))),
            "missing asset files must fail packaging"
        );
        assert!(
            !output_dir.exists(),
            "failed packaging must not create the output directory"
        );
    }

    #[test]
    fn missing_file_on_disk_produces_non_blocking_diagnostic() {
        let manifest = manifest_with("meshes/does_not_exist.obj");
        let report = analyze_build(&config_with_scene(), &manifest);
        assert!(
            report.diagnostics.iter().any(|d| matches!(
                &d.kind,
                BuildDiagnosticKind::MissingAsset { path } if path == "meshes/does_not_exist.obj"
            )),
            "missing file must produce a MissingAsset diagnostic"
        );
        assert!(
            !report.diagnostics.iter().any(|d| d.blocking),
            "missing file must be non-blocking in v1"
        );
        assert!(report.success, "missing file must not block the build");
    }

    /// Sets up a minimal packageable project (`project.json`,
    /// `project_settings.json`, a start scene) plus two independent imports of
    /// the same fixture glTF under different source IDs — enough to exercise
    /// ADR 0079's packaging bake walk without needing two handmade rigs (the
    /// two imports do not dedupe, since each is given an empty
    /// `existing_skeletons` ledger).
    fn project_with_two_independent_skinned_imports(
        project_root: &Path,
    ) -> (
        AssetId,
        engine::GltfImportResult,
        AssetId,
        engine::GltfImportResult,
    ) {
        std::fs::create_dir_all(project_root.join("assets/scenes")).expect("project dirs");
        std::fs::write(project_root.join("project.json"), "{}").expect("project.json fixture");
        std::fs::write(project_root.join("project_settings.json"), "{}").expect("settings fixture");
        std::fs::write(project_root.join("assets/scenes/main.scene.json"), "{}")
            .expect("scene fixture");

        let fixture_bytes = std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skinned_motion.gltf"),
        )
        .expect("skinned glTF fixture must exist");
        std::fs::write(project_root.join("assets/hero.gltf"), &fixture_bytes)
            .expect("hero fixture must write");
        std::fs::write(project_root.join("assets/villain.gltf"), &fixture_bytes)
            .expect("villain fixture must write");

        let hero_source = AssetId::generate();
        let villain_source = AssetId::generate();
        let hero_imported =
            engine::import_gltf_path(&hero_source, &project_root.join("assets/hero.gltf"), &[])
                .expect("hero fixture must import");
        let villain_imported = engine::import_gltf_path(
            &villain_source,
            &project_root.join("assets/villain.gltf"),
            &[],
        )
        .expect("villain fixture must import");

        (hero_source, hero_imported, villain_source, villain_imported)
    }

    /// Writes a `.scene.json` fixture with one entity carrying
    /// `engine.skeleton` (`source` / `skin` bound to `target_skin_source` /
    /// `target_skin_id`) and `engine.animator` (`clip_source` bound to
    /// `clip_id`) — the exact shape `collect_needed_retarget_pairs` (AP-7)
    /// reads to compute a needed `(source_skeleton, target_skeleton)` pair.
    /// Writes the ADR 0082 authoring shape used by new content. Unlike the
    /// legacy fixture above, the source model is intentionally not persisted
    /// beside the target Skin: packaging must derive its owner from the
    /// manifest in the same way runtime conversion does.
    /// Writes the current authoring shape for one animated character: a
    /// Skinned Model owning the rig built from `target_skeleton`, plus an
    /// Animation Controller whose Graph and Animation Set bind one motion slot
    /// to `clip` (ADR 0085, ADR 0087). This is the exact shape
    /// `collect_needed_retarget_pairs` (AP-7) reads to compute a needed
    /// `(source_skeleton, target_skeleton)` pair.
    ///
    /// Returns the `(graph, animation_set)` asset IDs, which the caller must
    /// register in its manifest under `animation/retarget.anim.graph.json` and
    /// `animation/retarget.animset.json`.
    fn write_animated_character_fixture(
        project_root: &Path,
        target_skeleton: &AssetId,
        clip: &AssetId,
    ) -> (AssetId, AssetId) {
        let graph_id = AssetId::generate();
        let set_id = AssetId::generate();
        let motion_slot = engine_authoring::id::MotionSlotId::generate();
        write_animation_graph_fixture(
            &project_root.join("assets/animation/retarget.anim.graph.json"),
            &motion_slot,
        );

        let mut animation_set = engine_authoring::AnimationSet::new(graph_id.clone());
        animation_set.bindings.insert(
            motion_slot,
            engine_authoring::AnimationBinding {
                name: "retarget_motion".to_owned(),
                clip: engine_authoring::MotionSourceRef::native(clip.clone()),
                overlays: Vec::new(),
                events: Vec::new(),
            },
        );
        std::fs::write(
            project_root.join("assets/animation/retarget.animset.json"),
            animation_set
                .to_canonical_json()
                .expect("animation set fixture must serialize"),
        )
        .expect("animation set fixture must write");

        let mut entity = engine_authoring::AuthoringEntity::new(
            engine_authoring::id::EntityId::generate(),
            "animated",
        );
        entity.components.insert(
            engine_authoring::id::ComponentTypeId::new(
                engine::scene_bridge::SKINNED_MODEL_COMPONENT,
            ),
            engine_authoring::Value::Object(BTreeMap::from([(
                "skeleton".to_owned(),
                engine_authoring::Value::AssetRef(target_skeleton.clone()),
            )])),
        );
        entity.components.insert(
            engine_authoring::id::ComponentTypeId::new(
                engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT,
            ),
            engine_authoring::Value::Object(BTreeMap::from([
                (
                    "animation_set".to_owned(),
                    engine_authoring::Value::AssetRef(set_id.clone()),
                ),
                (
                    "graph".to_owned(),
                    engine_authoring::Value::AssetRef(graph_id.clone()),
                ),
                ("looping".to_owned(), engine_authoring::Value::Bool(true)),
                (
                    "playback_speed".to_owned(),
                    engine_authoring::Value::F64(1.0),
                ),
                (
                    "completion_event".to_owned(),
                    engine_authoring::Value::String("animation.completed".to_owned()),
                ),
                (
                    "root_motion_mode".to_owned(),
                    engine_authoring::Value::String("disabled".to_owned()),
                ),
                (
                    "fade_duration".to_owned(),
                    engine_authoring::Value::F64(0.2),
                ),
                (
                    "parameters".to_owned(),
                    engine_authoring::Value::Object(BTreeMap::new()),
                ),
            ])),
        );

        // `AuthoringScene` mutation requires a `Transaction` from outside the
        // authoring crate; a scene document is just
        // `{ schema_version, entities }` (see `load.rs`'s `SceneFile`), so a
        // minimal local wrapper serializes straight to that shape without
        // needing scene-editing machinery for a test fixture.
        #[derive(serde::Serialize)]
        struct SceneFileFixture<'a> {
            schema_version: u32,
            entities: Vec<&'a engine_authoring::AuthoringEntity>,
        }
        let json = serde_json::to_string_pretty(&SceneFileFixture {
            schema_version: 1,
            entities: vec![&entity],
        })
        .expect("scene fixture must serialize");
        std::fs::write(project_root.join("assets/scenes/main.scene.json"), json)
            .expect("scene fixture must write");

        (graph_id, set_id)
    }

    /// Registers the graph and Animation Set written by
    /// [`write_animated_character_fixture`].
    fn register_animated_character_assets(
        manifest: &mut AssetManifest,
        graph_id: &AssetId,
        set_id: &AssetId,
    ) {
        for (id, path, name) in [
            (
                graph_id,
                "animation/retarget.anim.graph.json",
                "retarget_graph",
            ),
            (set_id, "animation/retarget.animset.json", "retarget_set"),
        ] {
            manifest.insert(
                id.clone(),
                ManifestEntry {
                    path: path.to_owned(),
                    name: Some(name.to_owned()),
                    import_settings: ImportSettings::default(),
                },
            );
        }
    }

    fn write_animation_graph_fixture(
        path: &Path,
        motion_slot: &engine_authoring::id::MotionSlotId,
    ) {
        let domain = AnimationGraphDomain::new();
        let mut graph = engine_authoring::Graph::new(
            engine_authoring::id::GraphId::generate(),
            domain.graph_kind().clone(),
            "retarget_graph",
        );
        let entry = engine_authoring::id::NodeId::generate();
        let state = engine_authoring::id::NodeId::generate();
        graph.nodes.insert(
            entry.clone(),
            Node::new(
                entry.clone(),
                domain.entry_type().clone(),
                engine_authoring::Value::Object(BTreeMap::new()),
            ),
        );
        graph.nodes.insert(
            state.clone(),
            Node::new(
                state.clone(),
                domain.state_type().clone(),
                engine_authoring::Value::Object(BTreeMap::from([
                    (
                        "motion_slot".to_owned(),
                        engine_authoring::Value::String(motion_slot.as_str().to_owned()),
                    ),
                    (
                        "motion_name".to_owned(),
                        engine_authoring::Value::String("retarget_motion".to_owned()),
                    ),
                ])),
            ),
        );
        graph.annotations.insert(
            engine_authoring::MOTION_SLOTS_ANNOTATION.to_owned(),
            engine_authoring::motion_slots_annotation_value(&[engine_authoring::MotionSlot {
                id: motion_slot.clone(),
                display_name: "retarget_motion".to_owned(),
            }]),
        );
        let edge_id = engine_authoring::id::EdgeId::generate();
        graph.edges.insert(
            edge_id.clone(),
            Edge::new(
                edge_id,
                PortRef::new(entry, domain.entry_out_port().clone()),
                PortRef::new(state, domain.state_in_port().clone()),
            ),
        );
        let json = graph
            .to_canonical_json(&domain)
            .expect("animation graph fixture must serialize");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("animation graph directory must exist");
        }
        std::fs::write(path, json).expect("animation graph fixture must write");
    }

    fn manifest_entry_for(
        path: &str,
        imported: &engine::GltfImportResult,
        fingerprint: String,
    ) -> ManifestEntry {
        ManifestEntry {
            path: path.into(),
            name: None,
            import_settings: ImportSettings {
                source_fingerprint: Some(fingerprint),
                sub_assets: imported.imported_sub_assets(),
                ..ImportSettings::default()
            },
        }
    }

    #[test]
    fn plan_package_bakes_and_copies_a_registered_retarget_map() {
        let dir = tempfile::tempdir().expect("temp dir");
        let project_root = dir.path().join("project");
        let (hero_source, hero_imported, villain_source, villain_imported) =
            project_with_two_independent_skinned_imports(&project_root);

        let hero_skeleton = hero_imported.skins[0].skeleton.clone();
        let villain_skeleton = villain_imported.skins[0].skeleton.clone();
        assert_ne!(
            hero_skeleton.id, villain_skeleton.id,
            "independently imported sources must not dedupe to the same skeleton id"
        );

        let mut map = engine::generate_retarget_map(&hero_skeleton, &villain_skeleton);
        assert!(!map.bone_pairs.is_empty());
        // Sidestep the fixture's lack of a root translation channel; see the
        // identical note in `scene_bridge::tests`.
        map.translation = engine::TranslationPolicy {
            mode: engine::TranslationMode::None,
            scale: engine::TranslationScale::Manual(1.0),
        };
        std::fs::write(
            project_root.join("assets/hero_to_villain.retarget.json"),
            map.to_json().expect("map must serialize"),
        )
        .expect("map fixture must write");

        // Overwrite the placeholder empty scene with an entity that actually
        // needs this pair: villain skeleton as the target, hero's clip as the
        // source (AP-7 reachability trace).
        let (graph_id, set_id) = write_animated_character_fixture(
            &project_root,
            &villain_imported.skins[0].skeleton_id,
            &hero_imported.animations[0].id,
        );

        let hero_fingerprint =
            engine::fingerprint_gltf_source(&project_root.join("assets/hero.gltf"), &[])
                .expect("hero fingerprint");
        let villain_fingerprint =
            engine::fingerprint_gltf_source(&project_root.join("assets/villain.gltf"), &[])
                .expect("villain fingerprint");

        let mut manifest = AssetManifest::default();
        manifest.insert(
            hero_source,
            manifest_entry_for("hero.gltf", &hero_imported, hero_fingerprint),
        );
        manifest.insert(
            villain_source,
            manifest_entry_for("villain.gltf", &villain_imported, villain_fingerprint),
        );
        register_animated_character_assets(&mut manifest, &graph_id, &set_id);
        manifest.insert(
            AssetId::generate(),
            ManifestEntry {
                path: "hero_to_villain.retarget.json".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );

        let config = BuildConfig {
            project_root: project_root.clone(),
            output_dir: dir.path().join("out"),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = plan_package(&config, &manifest);

        assert!(
            plan.success,
            "baking a resolvable retarget map must succeed: {:?}",
            plan.diagnostics
        );
        let baked_copies: Vec<_> = plan
            .copies
            .iter()
            .filter(|copy| copy.destination.starts_with("baked_anim"))
            .collect();
        assert!(
            !baked_copies.is_empty(),
            "at least one clip bound to the source rig must be baked"
        );
        for copy in &baked_copies {
            assert!(
                project_root.join(&copy.source).is_file(),
                "baked source {:?} must exist in the derived cache",
                copy.source
            );
        }
    }

    /// Key-parity guard (ADR 0079 §4 / AP-8): bakes a retarget through the
    /// real packaging path, stages its output the way its `PackageCopy`
    /// entries describe (`baked_anim/<file_name>`), then runs the engine's
    /// own cross-skeleton animator resolution against that staged directory
    /// with `PackagedBakedClips` inserted — exactly how the shipped player
    /// resolves it. Packaging and the player each compute the derived-cache
    /// key independently (`bake_registered_retarget_clips` here vs
    /// `resolve_cross_skeleton_clip` in `engine::scene_bridge`); if they ever
    /// diverge, the packaged lookup silently misses even though a
    /// differently-keyed file sits right next to the one it wanted, which is
    /// exactly the failure this test is designed to catch.
    #[test]
    fn packaged_player_resolves_a_packaging_baked_clip_with_a_matching_cache_key() {
        let dir = tempfile::tempdir().expect("temp dir");
        let project_root = dir.path().join("project");
        let (hero_source, hero_imported, villain_source, villain_imported) =
            project_with_two_independent_skinned_imports(&project_root);

        let hero_skeleton = hero_imported.skins[0].skeleton.clone();
        let villain_skeleton = villain_imported.skins[0].skeleton.clone();
        assert_ne!(
            hero_skeleton.id, villain_skeleton.id,
            "independently imported sources must not dedupe to the same skeleton id"
        );

        let mut map = engine::generate_retarget_map(&hero_skeleton, &villain_skeleton);
        assert!(!map.bone_pairs.is_empty());
        // Sidestep the fixture's lack of a root translation channel; see the
        // identical note in `plan_package_bakes_and_copies_a_registered_retarget_map`.
        map.translation = engine::TranslationPolicy {
            mode: engine::TranslationMode::None,
            scale: engine::TranslationScale::Manual(1.0),
        };
        std::fs::write(
            project_root.join("assets/hero_to_villain.retarget.json"),
            map.to_json().expect("map must serialize"),
        )
        .expect("map fixture must write");

        let (graph_id, set_id) = write_animated_character_fixture(
            &project_root,
            &villain_imported.skins[0].skeleton_id,
            &hero_imported.animations[0].id,
        );

        let hero_fingerprint =
            engine::fingerprint_gltf_source(&project_root.join("assets/hero.gltf"), &[])
                .expect("hero fingerprint");
        let villain_fingerprint =
            engine::fingerprint_gltf_source(&project_root.join("assets/villain.gltf"), &[])
                .expect("villain fingerprint");

        let mut manifest = AssetManifest::default();
        manifest.insert(
            hero_source.clone(),
            manifest_entry_for("hero.gltf", &hero_imported, hero_fingerprint),
        );
        manifest.insert(
            villain_source.clone(),
            manifest_entry_for("villain.gltf", &villain_imported, villain_fingerprint),
        );
        manifest.insert(
            AssetId::generate(),
            ManifestEntry {
                path: "hero_to_villain.retarget.json".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );
        register_animated_character_assets(&mut manifest, &graph_id, &set_id);

        let config = BuildConfig {
            project_root: project_root.clone(),
            output_dir: dir.path().join("out"),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = plan_package(&config, &manifest);
        assert!(
            plan.success,
            "baking a resolvable retarget map must succeed: {:?}",
            plan.diagnostics
        );
        let baked_copies: Vec<_> = plan
            .copies
            .iter()
            .filter(|copy| copy.destination.starts_with("baked_anim"))
            .collect();
        assert!(
            !baked_copies.is_empty(),
            "at least one clip bound to the source rig must be baked"
        );

        // Stage every baked copy exactly where its `PackageCopy` destination
        // says it belongs, under a fresh `baked_anim/` directory standing in
        // for `<package_root>/baked_anim`.
        let package_root = dir.path().join("package");
        let staged_baked_anim = package_root.join("baked_anim");
        std::fs::create_dir_all(&staged_baked_anim)
            .expect("staged baked_anim dir must be creatable");
        for copy in &baked_copies {
            let file_name = copy
                .destination
                .file_name()
                .expect("baked_anim copy destination must have a file name");
            std::fs::copy(
                project_root.join(&copy.source),
                staged_baked_anim.join(file_name),
            )
            .expect("staging a baked copy must succeed");
        }

        let scene_json =
            std::fs::read_to_string(project_root.join("assets/scenes/main.scene.json"))
                .expect("scene fixture must be readable");
        let scene =
            engine_authoring::load_scene_from_json(&scene_json).expect("scene fixture must load");

        let mut world = engine::ecs::World::new();
        world.insert_resource(engine::AssetServer::with_assets_root(
            project_root.join("assets"),
        ));
        world.insert_resource(manifest);
        world.insert_resource(engine::PackagedBakedClips {
            root: staged_baked_anim,
        });

        let bridge = engine::scene_bridge::spawn_from_authoring_scene(&mut world, &scene)
            .expect("packaged resolution must succeed against a correctly staged baked_anim/");
        assert!(
            !bridge.asset_diagnostics.iter().any(|d| {
                d.code == engine::RETARGET_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC
                    || d.code == engine::RETARGET_MAP_MISSING_DIAGNOSTIC
            }),
            "the packaged clip must resolve under the same key packaging baked it with: {:?}",
            bridge.asset_diagnostics
        );
        let entity = bridge
            .get(scene.entities().next().expect("authoring entity").0)
            .expect("runtime entity");
        let animator = world
            .get_component::<engine::Animator>(entity)
            .expect("animator must remain attached after a successful packaged resolution");
        let clip = world
            .get_resource::<engine::Assets<engine::AnimationClip>>()
            .and_then(|clips| clips.get(&animator.clip))
            .expect("resolved clip must exist");
        assert_eq!(
            clip.skeleton,
            Some(villain_skeleton.id),
            "the resolved clip must be the retargeted (villain-bound) clip loaded from the package"
        );
    }

    #[test]
    fn plan_package_blocks_when_a_registered_retarget_map_skeleton_is_unresolved() {
        let dir = tempfile::tempdir().expect("temp dir");
        let project_root = dir.path().join("project");
        let (hero_source, hero_imported, _villain_source, _villain_imported) =
            project_with_two_independent_skinned_imports(&project_root);

        let hero_skeleton = hero_imported.skins[0].skeleton.clone();
        // A target skeleton ID that matches no imported source's skin at all.
        let unresolvable_target = AssetId::generate();
        let map = engine::RetargetMap {
            schema_version: engine::RETARGET_MAP_SCHEMA_VERSION,
            source_skeleton: hero_skeleton.id.clone(),
            source_identity: hero_skeleton.identity.0,
            target_skeleton: unresolvable_target.clone(),
            target_identity: 0,
            bone_pairs: Vec::new(),
            chain_pairs: Vec::new(),
            translation: engine::TranslationPolicy::default(),
            // This test is about the skeleton-unresolved bake failure path,
            // not AP-7 reachability; `always_package` keeps the map in the
            // bake set without needing a scene fixture that references it.
            always_package: true,
        };
        std::fs::write(
            project_root.join("assets/hero_to_unknown.retarget.json"),
            map.to_json().expect("map must serialize"),
        )
        .expect("map fixture must write");

        let hero_fingerprint =
            engine::fingerprint_gltf_source(&project_root.join("assets/hero.gltf"), &[])
                .expect("hero fingerprint");
        let mut manifest = AssetManifest::default();
        manifest.insert(
            hero_source,
            manifest_entry_for("hero.gltf", &hero_imported, hero_fingerprint),
        );
        manifest.insert(
            AssetId::generate(),
            ManifestEntry {
                path: "hero_to_unknown.retarget.json".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );

        let config = BuildConfig {
            project_root: project_root.clone(),
            output_dir: dir.path().join("out"),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = plan_package(&config, &manifest);

        assert!(
            !plan.success,
            "an unresolvable retarget map skeleton must block packaging"
        );
        assert!(plan.copies.is_empty(), "failed plans must not list copies");
        assert!(plan.diagnostics.iter().any(|diagnostic| matches!(
            diagnostic.kind,
            BuildDiagnosticKind::RetargetSkeletonUnresolved { .. }
        )));
    }

    #[test]
    fn plan_package_blocks_when_a_registered_retarget_map_source_has_no_fingerprint() {
        let dir = tempfile::tempdir().expect("temp dir");
        let project_root = dir.path().join("project");
        let (hero_source, hero_imported, villain_source, villain_imported) =
            project_with_two_independent_skinned_imports(&project_root);

        let hero_skeleton = hero_imported.skins[0].skeleton.clone();
        let villain_skeleton = villain_imported.skins[0].skeleton.clone();
        assert_ne!(
            hero_skeleton.id, villain_skeleton.id,
            "independently imported sources must not dedupe to the same skeleton id"
        );

        let mut map = engine::generate_retarget_map(&hero_skeleton, &villain_skeleton);
        assert!(!map.bone_pairs.is_empty());
        map.translation = engine::TranslationPolicy {
            mode: engine::TranslationMode::None,
            scale: engine::TranslationScale::Manual(1.0),
        };
        // This test is about the unfingerprinted-source bake failure path,
        // not AP-7 reachability; `always_package` keeps the map in the bake
        // set without needing a scene fixture that references it.
        map.always_package = true;
        std::fs::write(
            project_root.join("assets/hero_to_villain.retarget.json"),
            map.to_json().expect("map must serialize"),
        )
        .expect("map fixture must write");

        let villain_fingerprint =
            engine::fingerprint_gltf_source(&project_root.join("assets/villain.gltf"), &[])
                .expect("villain fingerprint");

        let mut manifest = AssetManifest::default();
        // Deliberately no `source_fingerprint` on the hero entry, simulating
        // a source imported before fingerprints were recorded (AP-6).
        manifest.insert(
            hero_source,
            ManifestEntry {
                path: "hero.gltf".into(),
                name: None,
                import_settings: ImportSettings {
                    source_fingerprint: None,
                    sub_assets: hero_imported.imported_sub_assets(),
                    ..ImportSettings::default()
                },
            },
        );
        manifest.insert(
            villain_source,
            manifest_entry_for("villain.gltf", &villain_imported, villain_fingerprint),
        );
        manifest.insert(
            AssetId::generate(),
            ManifestEntry {
                path: "hero_to_villain.retarget.json".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );

        let config = BuildConfig {
            project_root: project_root.clone(),
            output_dir: dir.path().join("out"),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = plan_package(&config, &manifest);

        assert!(
            !plan.success,
            "an unfingerprinted retarget source must block packaging"
        );
        assert!(
            plan.copies.is_empty(),
            "failed plans must not list copies, including the baked_anim ones: {:?}",
            plan.copies
        );
        assert!(plan.diagnostics.iter().any(|diagnostic| matches!(
            &diagnostic.kind,
            BuildDiagnosticKind::RetargetSourceUnfingerprinted { .. }
        )));
    }

    // --- AP-7: reachability trace + always_package ---------------------------

    /// Builds the manifest + fixture files shared by the AP-7 tests below: two
    /// independently imported skinned sources, a hero -> villain retarget map
    /// (not yet given `always_package`), and the placeholder empty scene
    /// `project_with_two_independent_skinned_imports` already writes (so, by
    /// default, the map's pair is *not* needed by anything).
    fn ap7_fixture(
        project_root: &Path,
    ) -> (
        AssetManifest,
        engine::GltfImportResult,
        engine::GltfImportResult,
        AssetId,
        AssetId,
    ) {
        let (hero_source, hero_imported, villain_source, villain_imported) =
            project_with_two_independent_skinned_imports(project_root);

        let hero_skeleton = hero_imported.skins[0].skeleton.clone();
        let villain_skeleton = villain_imported.skins[0].skeleton.clone();
        let mut map = engine::generate_retarget_map(&hero_skeleton, &villain_skeleton);
        map.translation = engine::TranslationPolicy {
            mode: engine::TranslationMode::None,
            scale: engine::TranslationScale::Manual(1.0),
        };
        std::fs::write(
            project_root.join("assets/hero_to_villain.retarget.json"),
            map.to_json().expect("map must serialize"),
        )
        .expect("map fixture must write");

        let hero_fingerprint =
            engine::fingerprint_gltf_source(&project_root.join("assets/hero.gltf"), &[])
                .expect("hero fingerprint");
        let villain_fingerprint =
            engine::fingerprint_gltf_source(&project_root.join("assets/villain.gltf"), &[])
                .expect("villain fingerprint");

        let mut manifest = AssetManifest::default();
        manifest.insert(
            hero_source,
            manifest_entry_for("hero.gltf", &hero_imported, hero_fingerprint),
        );
        manifest.insert(
            villain_source.clone(),
            manifest_entry_for("villain.gltf", &villain_imported, villain_fingerprint),
        );
        let map_id = AssetId::generate();
        manifest.insert(
            map_id.clone(),
            ManifestEntry {
                path: "hero_to_villain.retarget.json".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );
        (
            manifest,
            hero_imported,
            villain_imported,
            villain_source,
            map_id,
        )
    }

    #[test]
    fn plan_package_skips_an_unreferenced_retarget_map_with_a_non_blocking_diagnostic() {
        let dir = tempfile::tempdir().expect("temp dir");
        let project_root = dir.path().join("project");
        // No scene or prefab references the hero -> villain pair at all: the
        // placeholder scene stays the empty fixture written by
        // `project_with_two_independent_skinned_imports`.
        let (manifest, ..) = ap7_fixture(&project_root);

        let config = BuildConfig {
            project_root: project_root.clone(),
            output_dir: dir.path().join("out"),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = plan_package(&config, &manifest);

        assert!(
            plan.success,
            "an unreferenced map must not block the build: {:?}",
            plan.diagnostics
        );
        assert!(
            !plan
                .copies
                .iter()
                .any(|copy| copy.destination.starts_with("baked_anim")),
            "an unreferenced map must not be baked: {:?}",
            plan.copies
        );
        assert!(
            plan.diagnostics.iter().any(|diagnostic| matches!(
                &diagnostic.kind,
                BuildDiagnosticKind::RetargetMapNotReached { path }
                    if path == "hero_to_villain.retarget.json"
            ) && !diagnostic.blocking),
            "an unreferenced map must report a non-blocking RetargetMapNotReached diagnostic: {:?}",
            plan.diagnostics
        );
    }

    #[test]
    fn plan_package_bakes_an_unreferenced_map_when_a_scene_references_its_pair() {
        let dir = tempfile::tempdir().expect("temp dir");
        let project_root = dir.path().join("project");
        let (mut manifest, hero_imported, villain_imported, _villain_source, _map_id) =
            ap7_fixture(&project_root);

        let (graph_id, set_id) = write_animated_character_fixture(
            &project_root,
            &villain_imported.skins[0].skeleton_id,
            &hero_imported.animations[0].id,
        );
        register_animated_character_assets(&mut manifest, &graph_id, &set_id);

        let config = BuildConfig {
            project_root: project_root.clone(),
            output_dir: dir.path().join("out"),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = plan_package(&config, &manifest);

        assert!(
            plan.success,
            "a map matching a scene-reached pair must bake: {:?}",
            plan.diagnostics
        );
        assert!(
            plan.copies
                .iter()
                .any(|copy| copy.destination.starts_with("baked_anim")),
            "a map matching a scene-reached pair must stage a baked clip: {:?}",
            plan.copies
        );
        assert!(
            !plan.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic.kind,
                BuildDiagnosticKind::RetargetMapNotReached { .. }
            )),
            "a reached map must not report RetargetMapNotReached: {:?}",
            plan.diagnostics
        );
    }

    #[test]
    fn plan_package_bakes_an_unreferenced_map_when_always_package_is_set() {
        let dir = tempfile::tempdir().expect("temp dir");
        let project_root = dir.path().join("project");
        let (manifest, ..) = ap7_fixture(&project_root);

        // Flip `always_package` on the already-written map file directly,
        // mirroring what the editor inspector's checkbox action writes. The
        // manifest entry keys the registered path, which is unaffected by
        // rewriting the file's contents.
        let map_path = project_root.join("assets/hero_to_villain.retarget.json");
        let mut map = engine::RetargetMap::from_json(
            &std::fs::read_to_string(&map_path).expect("map fixture must exist"),
        )
        .expect("map fixture must parse");
        map.always_package = true;
        std::fs::write(&map_path, map.to_json().expect("map must serialize"))
            .expect("map fixture must rewrite");

        let config = BuildConfig {
            project_root: project_root.clone(),
            output_dir: dir.path().join("out"),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = plan_package(&config, &manifest);

        assert!(
            plan.success,
            "an always_package map must bake even when unreferenced: {:?}",
            plan.diagnostics
        );
        assert!(
            plan.copies
                .iter()
                .any(|copy| copy.destination.starts_with("baked_anim")),
            "an always_package map must stage a baked clip: {:?}",
            plan.copies
        );
        assert!(
            !plan.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic.kind,
                BuildDiagnosticKind::RetargetMapNotReached { .. }
            )),
            "an always_package map must not report RetargetMapNotReached: {:?}",
            plan.diagnostics
        );
    }

    #[test]
    fn plan_package_bakes_a_retarget_map_referenced_only_by_a_prefab() {
        let dir = tempfile::tempdir().expect("temp dir");
        let project_root = dir.path().join("project");
        let (mut manifest, hero_imported, villain_imported, _villain_source, _map_id) =
            ap7_fixture(&project_root);

        std::fs::create_dir_all(project_root.join("assets/prefabs")).expect("prefabs dir");
        // The character lives only in a prefab, so the scene never mentions the
        // pair: the reachability walk has to find it through the prefab.
        let (graph_id, set_id) = write_animated_character_fixture(
            &project_root,
            &villain_imported.skins[0].skeleton_id,
            &hero_imported.animations[0].id,
        );
        register_animated_character_assets(&mut manifest, &graph_id, &set_id);
        let scene_source =
            std::fs::read_to_string(project_root.join("assets/scenes/main.scene.json"))
                .expect("fixture scene must read");
        let scene = engine_authoring::load_scene_from_json(&scene_source)
            .expect("fixture scene must parse");
        let mut prefab_entity = scene
            .entities()
            .next()
            .expect("fixture scene has one entity")
            .1
            .clone();
        prefab_entity.name = "mover".to_owned();
        // Only the prefab may reach the pair, so the scene goes back to empty.
        std::fs::write(
            project_root.join("assets/scenes/main.scene.json"),
            r#"{"schema_version":1,"entities":[]}"#,
        )
        .expect("empty scene must write");
        let prefab = engine_authoring::PrefabAsset {
            schema_version: engine_authoring::PREFAB_SCHEMA_VERSION,
            root: prefab_entity.id.clone(),
            entities: BTreeMap::from([(prefab_entity.id.clone(), prefab_entity)]),
        };
        std::fs::write(
            project_root.join("assets/prefabs/mover.prefab.json"),
            prefab.to_json().expect("prefab must serialize"),
        )
        .expect("prefab fixture must write");
        manifest.insert(
            AssetId::generate(),
            ManifestEntry {
                path: "prefabs/mover.prefab.json".into(),
                name: None,
                import_settings: ImportSettings::default(),
            },
        );

        let config = BuildConfig {
            project_root: project_root.clone(),
            output_dir: dir.path().join("out"),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = plan_package(&config, &manifest);

        assert!(
            plan.success,
            "a map matching a prefab-only reached pair must bake: {:?}",
            plan.diagnostics
        );
        assert!(
            plan.copies
                .iter()
                .any(|copy| copy.destination.starts_with("baked_anim")),
            "a map matching a prefab-only reached pair must stage a baked clip: {:?}",
            plan.copies
        );
    }

    #[test]
    fn plan_package_leaves_a_needed_pair_with_no_registered_map_unchanged() {
        // No retarget map is registered at all, only a scene entity that
        // needs a hero -> villain retarget: today's behavior (unchanged by
        // AP-7) is that packaging itself does not diagnose the missing map
        // (that already surfaces at editor Play / player resolution time as
        // `anim.retarget_map_missing`); the bake walk simply has nothing
        // registered to bake.
        let dir = tempfile::tempdir().expect("temp dir");
        let project_root = dir.path().join("project");
        let (hero_source, hero_imported, villain_source, villain_imported) =
            project_with_two_independent_skinned_imports(&project_root);

        let (graph_id, set_id) = write_animated_character_fixture(
            &project_root,
            &villain_imported.skins[0].skeleton_id,
            &hero_imported.animations[0].id,
        );

        let hero_fingerprint =
            engine::fingerprint_gltf_source(&project_root.join("assets/hero.gltf"), &[])
                .expect("hero fingerprint");
        let villain_fingerprint =
            engine::fingerprint_gltf_source(&project_root.join("assets/villain.gltf"), &[])
                .expect("villain fingerprint");
        let mut manifest = AssetManifest::default();
        manifest.insert(
            hero_source,
            manifest_entry_for("hero.gltf", &hero_imported, hero_fingerprint),
        );
        manifest.insert(
            villain_source,
            manifest_entry_for("villain.gltf", &villain_imported, villain_fingerprint),
        );
        register_animated_character_assets(&mut manifest, &graph_id, &set_id);

        let config = BuildConfig {
            project_root: project_root.clone(),
            output_dir: dir.path().join("out"),
            start_scene: Some("scenes/main.scene.json".into()),
        };

        let plan = plan_package(&config, &manifest);

        assert!(
            plan.success,
            "packaging does not itself diagnose a needed pair with no registered map: {:?}",
            plan.diagnostics
        );
        assert!(
            !plan
                .copies
                .iter()
                .any(|copy| copy.destination.starts_with("baked_anim")),
            "no map is registered, so nothing can be baked: {:?}",
            plan.copies
        );
        assert!(
            !plan.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic.kind,
                BuildDiagnosticKind::RetargetMapNotReached { .. }
            )),
            "with no registered map at all, there is nothing to report as unreached: {:?}",
            plan.diagnostics
        );
    }
}
