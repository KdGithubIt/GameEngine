//! Cancellable background import jobs for source assets.
//!
//! Parsing and image decoding can take long enough to make an immediate-mode
//! editor appear frozen. This module keeps that work off the UI thread and
//! sends only owned progress/result records back for main-thread persistence.

use engine::{
    asset::HumanoidProfile,
    humanoid_import::{build_humanoid_import_catalog, humanoid_imported_sub_assets},
    ImportedSubAsset, SkeletonRecord,
};
use engine_authoring::prefab::PrefabAsset;
use engine_authoring::{AssetId, Diagnostic};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;

/// Coarse import stage shown in the editor status UI.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetImportProgress {
    /// Normalized completion estimate in the inclusive `0..=1` range.
    pub fraction: f32,
    /// Human-readable stage label.
    pub stage: &'static str,
    /// Source file currently being processed.
    pub source_path: PathBuf,
}

/// One detected ground-contact interval with its bone resolved to a display
/// name (ADR 0080 §1, AP-5 Inspector display).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedContactInterval {
    /// Display name of the contact bone.
    pub bone_name: String,
    /// Clip-local start time in seconds, inclusive.
    pub start: f32,
    /// Clip-local end time in seconds, exclusive.
    pub end: f32,
}

/// Per-clip contact interval summary produced by one background import
/// (AP-5). Kept in memory only — re-detected fresh on every import, never
/// persisted, mirroring [`engine::AnimationClip::contacts`] itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipContactSummary {
    /// Sub-asset ID string of the animation clip this summary describes.
    pub clip_sub_asset_id: String,
    /// Human-readable clip name.
    pub clip_name: String,
    /// Detected intervals, bone IDs resolved to names against this import's
    /// own skeleton catalog.
    pub intervals: Vec<ResolvedContactInterval>,
}

/// Completed background model source (glTF/GLB or FBX) catalog operation.
#[derive(Debug)]
pub struct AssetImportResult {
    /// Project root captured when the job started.
    pub project_path: PathBuf,
    /// Stable top-level source asset ID.
    pub source_id: AssetId,
    /// Absolute source path used by the worker.
    pub source_path: PathBuf,
    /// Content fingerprint over source and external sidecars.
    pub source_fingerprint: Option<String>,
    /// Cheap metadata stamp captured by the successful import.
    pub source_stamp: Option<engine::SourceStamp>,
    /// Absolute external buffer/image dependency paths.
    pub source_dependencies: Vec<PathBuf>,
    /// Stable catalog produced by the latest successful parse.
    pub sub_assets: Vec<ImportedSubAsset>,
    /// Bone-catalog ledger for every skeleton this import bound to (ADR 0077),
    /// persisted into `ImportSettings::skeleton_records` alongside
    /// [`Self::sub_assets`].
    pub skeleton_records: Vec<SkeletonRecord>,
    /// Valid model-owned Humanoid profiles detected from this model import (ADR 0110).
    /// Motion-only sources leave this empty because the target model owns the profile.
    pub humanoid_profiles: Vec<HumanoidProfile>,
    /// Per-clip detected ground-contact intervals (ADR 0080 §1), reflecting
    /// whatever `contact_bones` override [`AssetImportManager::start_gltf`]
    /// was called with. Used by the Inspector's contact interval display
    /// (AP-5) so it never needs a second, separate parse of the source.
    pub animation_contacts: Vec<ClipContactSummary>,
    /// Placeable entity subtree rebuilt from this parse (ADR 0074).
    ///
    /// `None` when the source draws nothing, and on cancelled or failed
    /// jobs, so the previously generated prefab is left untouched.
    pub prefab: Option<PrefabAsset>,
    /// Non-fatal importer diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Fatal error, when import did not produce new catalog metadata.
    pub error: Option<String>,
    /// Whether cancellation was observed before publishing metadata.
    pub cancelled: bool,
}

enum WorkerMessage {
    Progress(AssetImportProgress),
    /// Boxed so one finished import, which carries a whole prefab, does not
    /// widen every progress message sent through the same channel.
    Complete(Box<AssetImportResult>),
}

#[derive(Debug, Clone)]
struct ActiveImport {
    project_path: PathBuf,
    source_id: AssetId,
    source_path: PathBuf,
}

/// Inputs for one background `*.vmd` motion import job
/// ([`AssetImportManager::start_vmd`]).
///
/// A struct rather than a parameter list because a motion job needs both the
/// motion and the model it is baked against, and passing seven positional
/// arguments — four of which are an `AssetId` or a `PathBuf` — makes the two
/// sources easy to transpose at a call site.
#[derive(Debug, Clone)]
pub struct MotionImportJob {
    /// Project root, captured when the job starts.
    pub project_path: PathBuf,
    /// The motion's own stable manifest [`AssetId`].
    pub source_id: AssetId,
    /// Absolute path of the `.vmd` file.
    pub source_path: PathBuf,
    /// Every PMX target selected for this motion, in stable authoring order.
    pub targets: Vec<MotionImportTarget>,
    /// Optional PMX whose MMD constraint rig is evaluated before the baked FK
    /// result is retargeted to [`Self::targets`]. `None` selects direct bake.
    pub original_model: Option<MotionImportTarget>,
    /// Registered explicit retarget maps available when `original_model` is
    /// different from an output target.
    pub retarget_maps: Vec<(AssetId, engine::RetargetMap)>,
    /// Every skeleton ledger already known in the project (ADR 0077), so the
    /// model import behind this bake dedupes against the same records the
    /// model's own import used.
    pub existing_skeletons: Vec<SkeletonRecord>,
    /// The motion's contact-bone override (ADR 0080 §1); empty keeps the
    /// default foot/ankle/toe heuristic.
    pub contact_bones: Vec<String>,
}

/// One target PMX used by a multi-target VMD import job.
#[derive(Debug, Clone)]
pub struct MotionImportTarget {
    /// Stable top-level asset ID of the PMX source.
    pub model_source_id: AssetId,
    /// Absolute path of the paired `.pmx` file.
    pub model_path: PathBuf,
    /// Contact-bone override owned by this output model. Retargeting detects
    /// contacts on the target rig, so this is distinct from the VMD override.
    pub contact_bones: Vec<String>,
}

/// Reports why a background import could not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetImportStartError {
    /// Another source is already being imported.
    AlreadyRunning,
}

impl fmt::Display for AssetImportStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => formatter.write_str("another asset import is already running"),
        }
    }
}

impl std::error::Error for AssetImportStartError {}

/// Owns at most one cooperative background asset import.
#[derive(Default)]
pub struct AssetImportManager {
    receiver: Option<Receiver<WorkerMessage>>,
    cancellation: Option<Arc<AtomicBool>>,
    progress: Option<AssetImportProgress>,
    active: Option<ActiveImport>,
}

impl fmt::Debug for AssetImportManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AssetImportManager")
            .field("progress", &self.progress)
            .finish()
    }
}

impl AssetImportManager {
    /// Returns whether a worker is currently active.
    pub fn is_running(&self) -> bool {
        self.receiver.is_some()
    }

    /// Returns the latest worker progress snapshot.
    pub fn progress(&self) -> Option<&AssetImportProgress> {
        self.progress.as_ref()
    }

    /// Returns the source currently being imported, if any.
    ///
    /// Callers queueing work deduplicate against this so a burst of
    /// filesystem events cannot re-queue the job already in flight.
    pub fn active_source(&self) -> Option<&AssetId> {
        self.active.as_ref().map(|active| &active.source_id)
    }

    /// Starts a model source catalog job (glTF/GLB or FBX, dispatched by
    /// extension per ADR 0081) without blocking the editor thread.
    ///
    /// `existing_skeletons` (ADR 0077) should be every skeleton bone-catalog
    /// record already known in the project — typically the union of every
    /// registered source's `ImportSettings::skeleton_records` — so the
    /// dedupe rule can recognize a rig imported from a different source.
    ///
    /// `existing_humanoid_profiles` is this model source's persisted Humanoid
    /// profile state. Valid authored mappings are reused and stale authored
    /// mappings are preserved instead of being silently replaced (ADR 0110).
    ///
    /// `contact_bones` (ADR 0080 §1, AP-5) is normally this source's own
    /// `ImportSettings::contact_bones`; an empty list keeps the default
    /// foot/ankle/toe name heuristic.
    ///
    /// # Errors
    ///
    /// Returns [`AssetImportStartError::AlreadyRunning`] when the single job
    /// slot is occupied.
    pub fn start_gltf(
        &mut self,
        project_path: PathBuf,
        source_id: AssetId,
        source_path: PathBuf,
        existing_skeletons: Vec<SkeletonRecord>,
        existing_humanoid_profiles: Vec<HumanoidProfile>,
        contact_bones: Vec<String>,
    ) -> Result<(), AssetImportStartError> {
        if self.is_running() {
            return Err(AssetImportStartError::AlreadyRunning);
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.cancellation = Some(cancellation);
        self.progress = Some(AssetImportProgress {
            fraction: 0.0,
            stage: "Queued",
            source_path: source_path.clone(),
        });
        self.active = Some(ActiveImport {
            project_path: project_path.clone(),
            source_id: source_id.clone(),
            source_path: source_path.clone(),
        });

        thread::spawn(move || {
            let progress = |fraction, stage| {
                let _ = sender.send(WorkerMessage::Progress(AssetImportProgress {
                    fraction,
                    stage,
                    source_path: source_path.clone(),
                }));
            };
            progress(0.05, "Reading source");
            if worker_cancellation.load(Ordering::Acquire) {
                send_cancelled(&sender, project_path, source_id, source_path);
                return;
            }
            progress(0.20, "Resolving buffers and images");
            let imported = match engine::import_model_path_with_contact_bones(
                &source_id,
                &source_path,
                &existing_skeletons,
                &contact_bones,
            ) {
                Ok(imported) => imported,
                Err(error) => {
                    send_failed(
                        &sender,
                        project_path,
                        source_id,
                        source_path,
                        error.to_string(),
                    );
                    return;
                }
            };
            if worker_cancellation.load(Ordering::Acquire) {
                send_cancelled(&sender, project_path, source_id, source_path);
                return;
            }
            progress(0.75, "Cataloging sub-assets");
            let dependencies = match engine::model_source_dependencies(&source_path) {
                Ok(dependencies) => dependencies,
                Err(error) => {
                    send_failed(
                        &sender,
                        project_path,
                        source_id,
                        source_path,
                        error.to_string(),
                    );
                    return;
                }
            };
            let fingerprint = match engine::fingerprint_model_source(&source_path, &dependencies) {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    send_failed(
                        &sender,
                        project_path,
                        source_id,
                        source_path,
                        error.to_string(),
                    );
                    return;
                }
            };
            let source_stamp = match engine::SourceStamp::capture(&source_path, &dependencies) {
                Ok(stamp) => stamp,
                Err(error) => {
                    send_failed(
                        &sender,
                        project_path,
                        source_id,
                        source_path,
                        error.to_string(),
                    );
                    return;
                }
            };
            if worker_cancellation.load(Ordering::Acquire) {
                send_cancelled(&sender, project_path, source_id, source_path);
                return;
            }
            progress(0.90, "Building placement prefab");
            let prefab_name = source_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("model")
                .to_owned();
            let prefab = engine::build_gltf_prefab(&source_id, &imported, &prefab_name);
            progress(0.95, "Publishing import result");
            let mut humanoid_catalog =
                build_humanoid_import_catalog(&imported, &existing_humanoid_profiles);
            let mut sub_assets = imported.imported_sub_assets();
            sub_assets.extend(humanoid_imported_sub_assets(&humanoid_catalog));
            let skeleton_records = imported.skeleton_records.clone();
            let animation_contacts = resolve_animation_contacts(&imported);
            let _ = sender.send(WorkerMessage::Complete(Box::new(AssetImportResult {
                project_path,
                source_id,
                source_path,
                source_fingerprint: Some(fingerprint),
                source_stamp: Some(source_stamp),
                source_dependencies: dependencies,
                sub_assets,
                skeleton_records,
                humanoid_profiles: humanoid_catalog.profiles,
                animation_contacts,
                prefab,
                diagnostics: {
                    let mut diagnostics = imported.diagnostics;
                    diagnostics.append(&mut humanoid_catalog.diagnostics);
                    diagnostics
                },
                error: None,
                cancelled: false,
            })));
        });
        Ok(())
    }

    /// Starts a `*.vmd` motion source catalog job, baking it against the PMX
    /// model it is paired with (ADR 0097 §3), without blocking the editor
    /// thread.
    ///
    /// Each target identifies one registered PMX from this source's
    /// `ImportSettings::motion_model_sources`. The model's own ID (not the
    /// motion's) is passed through because skeleton sub-asset IDs derive from
    /// the source that owns the rig.
    ///
    /// The result carries only Animation sub-assets and no prefab: a motion
    /// draws nothing, so there is no placement subtree to regenerate. Its
    /// `skeleton_records` repeat the *model's* ledger for the bound skeleton,
    /// so retarget tooling (ADR 0079) can find which rig a baked motion
    /// belongs to from the motion's own manifest entry.
    ///
    /// # Errors
    ///
    /// Returns [`AssetImportStartError::AlreadyRunning`] when the single job
    /// slot is occupied.
    pub fn start_vmd(&mut self, job: MotionImportJob) -> Result<(), AssetImportStartError> {
        let MotionImportJob {
            project_path,
            source_id,
            source_path,
            targets,
            original_model,
            retarget_maps,
            existing_skeletons,
            contact_bones,
        } = job;
        if self.is_running() {
            return Err(AssetImportStartError::AlreadyRunning);
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.cancellation = Some(cancellation);
        self.progress = Some(AssetImportProgress {
            fraction: 0.0,
            stage: "Queued",
            source_path: source_path.clone(),
        });
        self.active = Some(ActiveImport {
            project_path: project_path.clone(),
            source_id: source_id.clone(),
            source_path: source_path.clone(),
        });

        thread::spawn(move || {
            let progress = |fraction, stage| {
                let _ = sender.send(WorkerMessage::Progress(AssetImportProgress {
                    fraction,
                    stage,
                    source_path: source_path.clone(),
                }));
            };
            let options = engine::VmdBakeOptions {
                contact_bone_names: contact_bones,
                ..engine::VmdBakeOptions::default()
            };
            // Populate the same project-derived cache that Scene View uses.
            // The import worker already performs the expensive bake, so a
            // later Animation Set assignment can deserialize it immediately
            // instead of repeating the work on the editor UI thread.
            let derived_cache = engine::DerivedCache::new(&project_path);
            let mut model_paths = targets
                .iter()
                .map(|target| target.model_path.clone())
                .collect::<Vec<_>>();
            if let Some(original) = &original_model {
                model_paths.push(original.model_path.clone());
            }
            let mut sub_assets = Vec::new();
            let mut skeleton_records = Vec::new();
            let mut animation_contacts = Vec::new();
            let mut diagnostics = Vec::new();
            if let Some(original) = &original_model {
                progress(0.05, "Reading original PMX model");
                let original_rig = match engine::VmdBakeRig::from_model_path(
                    &original.model_source_id,
                    &original.model_path,
                    &existing_skeletons,
                ) {
                    Ok(rig) => rig,
                    Err(error) => {
                        send_failed(
                            &sender,
                            project_path,
                            source_id,
                            source_path,
                            error.to_string(),
                        );
                        return;
                    }
                };
                progress(0.15, "Baking motion against original PMX");
                let mut original_bake = match engine::resolve_or_bake_vmd_path(
                    &derived_cache,
                    &source_id,
                    &source_path,
                    &original_rig,
                    &options,
                ) {
                    Ok(baked) => baked,
                    Err(error) => {
                        send_failed(
                            &sender,
                            project_path,
                            source_id,
                            source_path,
                            error.to_string(),
                        );
                        return;
                    }
                };
                diagnostics.append(&mut original_bake.diagnostics);

                for (target_index, target) in targets.iter().enumerate() {
                    if worker_cancellation.load(Ordering::Acquire) {
                        send_cancelled(&sender, project_path, source_id, source_path);
                        return;
                    }
                    let target_fraction = target_index as f32 / targets.len().max(1) as f32;
                    if target.model_source_id == original.model_source_id {
                        let mut baked = original_bake.clone();
                        baked.bind_model_source(&source_id, &target.model_source_id);
                        append_motion_catalog(
                            &mut baked,
                            original_rig.skeleton(),
                            &existing_skeletons,
                            &mut sub_assets,
                            &mut skeleton_records,
                            &mut animation_contacts,
                            &mut diagnostics,
                        );
                        continue;
                    }

                    progress(0.25 + target_fraction * 0.55, "Reading output PMX model");
                    let target_rig = match engine::VmdBakeRig::from_model_path(
                        &target.model_source_id,
                        &target.model_path,
                        &existing_skeletons,
                    ) {
                        Ok(rig) => rig,
                        Err(error) => {
                            send_failed(
                                &sender,
                                project_path,
                                source_id,
                                source_path,
                                error.to_string(),
                            );
                            return;
                        }
                    };
                    let Some(map) = engine::find_retarget_map_for_pair(
                        &retarget_maps,
                        &original_rig.skeleton().id,
                        &target_rig.skeleton().id,
                    ) else {
                        send_failed(
                            &sender,
                            project_path,
                            source_id,
                            source_path,
                            format!(
                                "no Retarget Map resolves original skeleton `{}` to output skeleton `{}`",
                                original_rig.skeleton().id.as_str(),
                                target_rig.skeleton().id.as_str()
                            ),
                        );
                        return;
                    };
                    if let Some(stale) = map
                        .validate(
                            original_rig.skeleton().identity,
                            target_rig.skeleton().identity,
                        )
                        .into_iter()
                        .next()
                    {
                        send_failed(
                            &sender,
                            project_path,
                            source_id,
                            source_path,
                            stale.message,
                        );
                        return;
                    }
                    progress(0.35 + target_fraction * 0.55, "Retargeting baked motion");
                    let mut baked = original_bake.clone();
                    if let Err(error) = baked.retarget_to_model_source(
                        &source_id,
                        &target.model_source_id,
                        original_rig.skeleton(),
                        target_rig.skeleton(),
                        map,
                        &target.contact_bones,
                    ) {
                        send_failed(
                            &sender,
                            project_path,
                            source_id,
                            source_path,
                            error.to_string(),
                        );
                        return;
                    }
                    append_motion_catalog(
                        &mut baked,
                        target_rig.skeleton(),
                        &existing_skeletons,
                        &mut sub_assets,
                        &mut skeleton_records,
                        &mut animation_contacts,
                        &mut diagnostics,
                    );
                }
            } else {
                for (target_index, target) in targets.iter().enumerate() {
                    if worker_cancellation.load(Ordering::Acquire) {
                        send_cancelled(&sender, project_path, source_id, source_path);
                        return;
                    }
                    let target_fraction = target_index as f32 / targets.len().max(1) as f32;
                    progress(0.05 + target_fraction * 0.70, "Reading target PMX model");
                    let rig = match engine::VmdBakeRig::from_model_path(
                        &target.model_source_id,
                        &target.model_path,
                        &existing_skeletons,
                    ) {
                        Ok(rig) => rig,
                        Err(error) => {
                            send_failed(
                                &sender,
                                project_path,
                                source_id,
                                source_path,
                                error.to_string(),
                            );
                            return;
                        }
                    };
                    progress(0.15 + target_fraction * 0.70, "Baking target motion");
                    let mut baked = match engine::resolve_or_bake_vmd_path(
                        &derived_cache,
                        &source_id,
                        &source_path,
                        &rig,
                        &options,
                    ) {
                        Ok(baked) => baked,
                        Err(error) => {
                            send_failed(
                                &sender,
                                project_path,
                                source_id,
                                source_path,
                                error.to_string(),
                            );
                            return;
                        }
                    };
                    baked.bind_model_source(&source_id, &target.model_source_id);
                    append_motion_catalog(
                        &mut baked,
                        rig.skeleton(),
                        &existing_skeletons,
                        &mut sub_assets,
                        &mut skeleton_records,
                        &mut animation_contacts,
                        &mut diagnostics,
                    );
                }
            }
            skeleton_records.sort_by(|left, right| left.id.cmp(&right.id));
            skeleton_records.dedup_by(|left, right| left.id == right.id);
            progress(0.85, "Cataloging target clips");
            let fingerprint = match engine::fingerprint_motion_sources(&source_path, &model_paths) {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    send_failed(
                        &sender,
                        project_path,
                        source_id,
                        source_path,
                        error.to_string(),
                    );
                    return;
                }
            };
            let source_dependencies = engine::motion_source_dependencies_for_models(&model_paths);
            let source_stamp = match engine::SourceStamp::capture(
                &source_path,
                &source_dependencies,
            ) {
                Ok(stamp) => stamp,
                Err(error) => {
                    send_failed(
                        &sender,
                        project_path,
                        source_id,
                        source_path,
                        error.to_string(),
                    );
                    return;
                }
            };
            progress(0.95, "Publishing import result");
            let _ = sender.send(WorkerMessage::Complete(Box::new(AssetImportResult {
                project_path,
                source_id,
                source_path,
                source_fingerprint: Some(fingerprint),
                source_stamp: Some(source_stamp),
                source_dependencies,
                sub_assets,
                skeleton_records,
                humanoid_profiles: Vec::new(),
                animation_contacts,
                // A motion draws nothing, so there is no placement prefab.
                prefab: None,
                diagnostics,
                error: None,
                cancelled: false,
            })));
        });
        Ok(())
    }

    /// Polls progress and returns a completed result without blocking.
    pub fn poll(&mut self) -> Option<AssetImportResult> {
        let receiver = self.receiver.as_ref()?;
        let mut completed = None;
        loop {
            match receiver.try_recv() {
                Ok(WorkerMessage::Progress(progress)) => self.progress = Some(progress),
                Ok(WorkerMessage::Complete(result)) => {
                    completed = Some(*result);
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if let Some(active) = self.active.take() {
                        completed = Some(AssetImportResult {
                            project_path: active.project_path,
                            source_id: active.source_id,
                            source_path: active.source_path,
                            source_fingerprint: None,
                            source_stamp: None,
                            source_dependencies: Vec::new(),
                            sub_assets: Vec::new(),
                            skeleton_records: Vec::new(),
                            humanoid_profiles: Vec::new(),
                            animation_contacts: Vec::new(),
                            prefab: None,
                            diagnostics: Vec::new(),
                            error: Some("asset import worker stopped unexpectedly".into()),
                            cancelled: false,
                        });
                    }
                    break;
                }
            }
        }
        if completed.is_some() {
            self.receiver = None;
            self.cancellation = None;
            self.progress = None;
            self.active = None;
        }
        completed
    }

    /// Requests cooperative cancellation of the current parse/decode job.
    pub fn cancel(&self) -> bool {
        let Some(cancellation) = &self.cancellation else {
            return false;
        };
        cancellation.store(true, Ordering::Release);
        true
    }
}

impl Drop for AssetImportManager {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}

/// Builds the AP-5 Inspector's per-clip contact interval summary from a
/// finished [`engine::GltfImportResult`], resolving each
/// [`engine::ContactInterval`]'s [`engine::BoneId`] against the clip's own
/// bound skeleton (ADR 0080 §1). A bone no longer present in that skeleton
/// (should not normally happen within one import) falls back to a `bone#N`
/// placeholder rather than panicking.
fn resolve_animation_contacts(imported: &engine::GltfImportResult) -> Vec<ClipContactSummary> {
    imported
        .animations
        .iter()
        .map(|animation| {
            let skeleton = imported
                .skins
                .get(animation.skin_index)
                .map(|skin| &skin.skeleton);
            let intervals = animation
                .clip
                .contacts
                .iter()
                .map(|interval| {
                    let bone_name = skeleton
                        .and_then(|skeleton| {
                            skeleton
                                .bone_index(interval.bone)
                                .map(|index| skeleton.bones[index].name.clone())
                        })
                        .unwrap_or_else(|| format!("bone#{}", interval.bone.0));
                    ResolvedContactInterval {
                        bone_name,
                        start: interval.start,
                        end: interval.end,
                    }
                })
                .collect();
            ClipContactSummary {
                clip_sub_asset_id: animation.id.as_str().to_owned(),
                clip_name: animation.name.clone(),
                intervals,
            }
        })
        .collect()
}

/// Resolves a baked motion's contact intervals against the one skeleton
/// every clip in it binds to (ADR 0080 §1), the motion-source counterpart of
/// [`resolve_animation_contacts`].
///
/// Simpler than the model-source version because a motion has no per-clip
/// skin index to look through: every clip it produces targets the rig it was
/// baked against.
fn resolve_motion_contacts(
    baked: &engine::VmdImportResult,
    skeleton: &engine::SkeletonAsset,
) -> Vec<ClipContactSummary> {
    baked
        .clips
        .iter()
        .map(|clip| ClipContactSummary {
            clip_sub_asset_id: clip.id.as_str().to_owned(),
            clip_name: clip.name.clone(),
            intervals: clip
                .clip
                .contacts
                .iter()
                .map(|interval| ResolvedContactInterval {
                    bone_name: skeleton
                        .bone_index(interval.bone)
                        .map(|index| skeleton.bones[index].name.clone())
                        .unwrap_or_else(|| format!("bone#{}", interval.bone.0)),
                    start: interval.start,
                    end: interval.end,
                })
                .collect(),
        })
        .collect()
}

/// Appends one completed target clip to the background import's stable
/// catalogs.
///
/// Both direct bakes and original-PMX retargets converge here so catalog,
/// contact, skeleton-ledger, and diagnostic behavior cannot drift between
/// the two processing paths.
fn append_motion_catalog(
    baked: &mut engine::VmdImportResult,
    skeleton: &engine::SkeletonAsset,
    existing_skeletons: &[SkeletonRecord],
    sub_assets: &mut Vec<ImportedSubAsset>,
    skeleton_records: &mut Vec<SkeletonRecord>,
    animation_contacts: &mut Vec<ClipContactSummary>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    animation_contacts.extend(resolve_motion_contacts(baked, skeleton));
    skeleton_records.extend(
        existing_skeletons
            .iter()
            .filter(|record| record.id == skeleton.id.as_str())
            .cloned(),
    );
    sub_assets.extend(baked.imported_sub_assets());
    diagnostics.append(&mut baked.diagnostics);
}

fn send_cancelled(
    sender: &mpsc::Sender<WorkerMessage>,
    project_path: PathBuf,
    source_id: AssetId,
    source_path: PathBuf,
) {
    let _ = sender.send(WorkerMessage::Complete(Box::new(AssetImportResult {
        project_path,
        source_id,
        source_path,
        source_fingerprint: None,
        source_stamp: None,
        source_dependencies: Vec::new(),
        sub_assets: Vec::new(),
        skeleton_records: Vec::new(),
        humanoid_profiles: Vec::new(),
        animation_contacts: Vec::new(),
        prefab: None,
        diagnostics: Vec::new(),
        error: None,
        cancelled: true,
    })));
}

fn send_failed(
    sender: &mpsc::Sender<WorkerMessage>,
    project_path: PathBuf,
    source_id: AssetId,
    source_path: PathBuf,
    error: String,
) {
    let _ = sender.send(WorkerMessage::Complete(Box::new(AssetImportResult {
        project_path,
        source_id,
        source_path,
        source_fingerprint: None,
        source_stamp: None,
        source_dependencies: Vec::new(),
        sub_assets: Vec::new(),
        skeleton_records: Vec::new(),
        humanoid_profiles: Vec::new(),
        animation_contacts: Vec::new(),
        prefab: None,
        diagnostics: Vec::new(),
        error: Some(error),
        cancelled: false,
    })));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One triangle skinned to a two-joint skeleton ("root_joint" ->
    /// "tip_joint") with a rotation clip on the tip joint (mirrors
    /// `engine::gltf_import::tests::SKINNED_GLTF`, copied here because that
    /// constant is crate-private to `engine`). `tip_joint`'s local
    /// translation never animates, so its model-space position stays
    /// constant (its parent, `root_joint`, has no channels either) for the
    /// whole 1-second clip: trivially "planted" for contact detection.
    /// Neither joint name matches the default foot/ankle/toe heuristic, which
    /// makes this fixture a clean probe for the `contact_bones` override.
    const SKINNED_RIG_FIXTURE: &str = r#"{
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 2]}],
        "nodes": [
            {"name": "root_joint", "children": [1]},
            {"name": "tip_joint", "translation": [0.0, 1.0, 0.0]},
            {"name": "character", "mesh": 0, "skin": 0}
        ],
        "skins": [{
            "name": "skeleton",
            "joints": [0, 1],
            "inverseBindMatrices": 3
        }],
        "meshes": [{
            "name": "triangle",
            "primitives": [{
                "attributes": {"POSITION": 0, "JOINTS_0": 1, "WEIGHTS_0": 2}
            }]
        }],
        "animations": [{
            "name": "spin",
            "channels": [
                {"sampler": 0, "target": {"node": 1, "path": "rotation"}},
                {"sampler": 1, "target": {"node": 2, "path": "translation"}}
            ],
            "samplers": [
                {"input": 4, "output": 5, "interpolation": "LINEAR"},
                {"input": 4, "output": 6, "interpolation": "LINEAR"}
            ]
        }],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
            {"bufferView": 1, "componentType": 5123, "count": 3, "type": "VEC4"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC4"},
            {"bufferView": 3, "componentType": 5126, "count": 2, "type": "MAT4"},
            {"bufferView": 4, "componentType": 5126, "count": 2, "type": "SCALAR",
             "min": [0.0], "max": [1.0]},
            {"bufferView": 5, "componentType": 5126, "count": 2, "type": "VEC4"},
            {"bufferView": 0, "componentType": 5126, "count": 2, "type": "VEC3",
             "min": [0.0, 0.0, 0.0], "max": [1.0, 0.0, 0.0]}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 24},
            {"buffer": 0, "byteOffset": 60, "byteLength": 48},
            {"buffer": 0, "byteOffset": 108, "byteLength": 128},
            {"buffer": 0, "byteOffset": 236, "byteLength": 8},
            {"buffer": 0, "byteOffset": 244, "byteLength": 32}
        ],
        "buffers": [{
            "byteLength": 276,
            "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAABAAAAAAAAAAEAAAAAAAAAAQAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAD8AAAA/AAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAAAAAAAAAAAAACAPwAAgD8AAAAAAAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAAAAAAAAgD8AAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAPMENT/zBDU/"
        }]
    }"#;

    #[test]
    fn contact_bones_override_is_threaded_through_the_editor_import_path() {
        let directory = tempfile::tempdir().expect("temporary import project");
        let source_path = directory.path().join("rig.gltf");
        std::fs::write(&source_path, SKINNED_RIG_FIXTURE).expect("fixture write");
        let source_id = AssetId::generate();

        let mut manager = AssetImportManager::default();
        manager
            .start_gltf(
                directory.path().to_path_buf(),
                source_id.clone(),
                source_path.clone(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .expect("import without override starts");
        let without_override = wait_for_result(&mut manager);
        assert_eq!(without_override.animation_contacts.len(), 1);
        assert!(
            without_override.animation_contacts[0].intervals.is_empty(),
            "neither root_joint nor tip_joint matches the default foot/ankle/toe heuristic, got {:?}",
            without_override.animation_contacts[0].intervals
        );

        manager
            .start_gltf(
                directory.path().to_path_buf(),
                source_id,
                source_path,
                without_override.skeleton_records.clone(),
                without_override.humanoid_profiles.clone(),
                vec!["tip_joint".to_owned()],
            )
            .expect("import with override starts");
        let with_override = wait_for_result(&mut manager);

        assert_eq!(with_override.animation_contacts.len(), 1);
        let intervals = &with_override.animation_contacts[0].intervals;
        assert_eq!(
            intervals.len(),
            1,
            "the override must select tip_joint even though its name matches no default pattern, got {intervals:?}"
        );
        assert_eq!(intervals[0].bone_name, "tip_joint");
        assert!(
            intervals[0].start <= 0.0 && intervals[0].end >= 1.0 - 1.0e-3,
            "tip_joint never moves in this fixture, so the whole clip must be one contact interval, got {:?}",
            intervals[0]
        );
    }

    #[test]
    fn resolve_animation_contacts_lists_intervals_with_resolved_bone_names() {
        let directory = tempfile::tempdir().expect("temporary import project");
        let source_path = directory.path().join("rig.gltf");
        std::fs::write(&source_path, SKINNED_RIG_FIXTURE).expect("fixture write");
        let source = AssetId::generate();

        let imported = engine::import_gltf_path_with_contact_bones(
            &source,
            &source_path,
            &[],
            &["tip_joint".to_owned()],
        )
        .expect("import must succeed");

        let summaries = resolve_animation_contacts(&imported);

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].clip_name, "spin");
        assert_eq!(summaries[0].intervals.len(), 1);
        assert_eq!(summaries[0].intervals[0].bone_name, "tip_joint");
        assert!((summaries[0].intervals[0].start - 0.0).abs() < 1.0e-6);
        assert!(summaries[0].intervals[0].end >= 1.0 - 1.0e-3);
    }

    #[test]
    fn catalog_keeps_source_selectors_in_stable_ids() {
        let source = AssetId::generate();
        let json = br#"{
            "asset": { "version": "2.0" },
            "meshes": [
                { "name": "first", "primitives": [] },
                { "name": "second", "primitives": [] }
            ]
        }"#;
        let imported = engine::import_gltf_bytes(&source, json, &[]).expect("valid glTF");
        // Empty primitives do not create runtime mesh assets, so no unstable
        // compacted indices may accidentally appear in the persisted catalog.
        assert!(imported.imported_sub_assets().is_empty());
    }

    #[test]
    fn start_rejects_a_second_running_job() {
        let mut manager = AssetImportManager::default();
        manager.receiver = Some(mpsc::channel().1);
        let result = manager.start_gltf(
            PathBuf::from("project"),
            AssetId::generate(),
            PathBuf::from("asset.glb"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(result, Err(AssetImportStartError::AlreadyRunning));
    }

    #[test]
    fn source_path_is_carried_by_progress() {
        let path = std::path::Path::new("character.glb");
        let progress = AssetImportProgress {
            fraction: 0.5,
            stage: "Parsing",
            source_path: path.to_path_buf(),
        };
        assert_eq!(progress.source_path, path);
    }

    #[test]
    fn reimport_changes_fingerprint_without_changing_sub_asset_id() {
        let directory = tempfile::tempdir().expect("temporary import project");
        let source_path = directory.path().join("hero.gltf");
        let mut positions = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            positions.extend_from_slice(&value.to_le_bytes());
        }
        std::fs::write(directory.path().join("hero.bin"), positions).expect("buffer fixture");
        let document = |name: &str| {
            format!(
                r#"{{
                    "asset":{{"version":"2.0"}},
                    "buffers":[{{"uri":"hero.bin","byteLength":36}}],
                    "bufferViews":[{{"buffer":0,"byteLength":36}}],
                    "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}}],
                    "meshes":[{{"name":"{name}","primitives":[{{"attributes":{{"POSITION":0}}}}]}}]
                }}"#
            )
        };
        std::fs::write(&source_path, document("Body")).expect("first source fixture");
        let source_id = AssetId::generate();
        let mut manager = AssetImportManager::default();
        manager
            .start_gltf(
                directory.path().to_path_buf(),
                source_id.clone(),
                source_path.clone(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .expect("first import starts");
        let first = wait_for_result(&mut manager);
        std::fs::write(&source_path, document("Renamed Body")).expect("second source fixture");
        manager
            .start_gltf(
                directory.path().to_path_buf(),
                source_id,
                source_path,
                first.skeleton_records.clone(),
                first.humanoid_profiles.clone(),
                Vec::new(),
            )
            .expect("reimport starts");
        let second = wait_for_result(&mut manager);

        assert_eq!(first.sub_assets.len(), 1);
        assert_eq!(second.sub_assets.len(), 1);
        assert_eq!(first.sub_assets[0].id, second.sub_assets[0].id);
        assert_ne!(first.source_fingerprint, second.source_fingerprint);
        assert_eq!(second.sub_assets[0].name, "Renamed Body");
    }

    fn wait_for_result(manager: &mut AssetImportManager) -> AssetImportResult {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(result) = manager.poll() {
                return result;
            }
            assert!(std::time::Instant::now() < deadline, "import timed out");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}
