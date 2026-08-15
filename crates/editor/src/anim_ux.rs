//! Editor-side observability UX for the animation retarget pipeline (AP-5,
//! `docs/ANIMATION_PIPELINE_PLAN.md`).
//!
//! The design-review requirement this module exists to satisfy is "never
//! silently match, never silently drop": bone binding (ADR 0077), retarget
//! mapping (ADR 0079), and contact intervals (ADR 0080) must all be visible
//! and correctable from the editor. Every type and function here is a pure,
//! toolkit-independent data model or builder (state -> widget model), mirroring
//! `crate::material_editor`'s split between data and rendering so the logic
//! stays testable without egui; thin `egui` rendering helpers live at the
//! bottom of this module and in `crate::ui::assets`.
//!
//! No new persisted format is introduced: every builder here reads
//! `engine::SkeletonRecord` / `engine::RetargetMap` / `engine::ImportSettings`
//! data that AP-1/AP-3/AP-4 already persist.

use eframe::egui;
use engine::glam::{Quat, Vec3};
use engine::{
    BoneDef, BoneId, BonePair, ImportedSubAssetKind, RetargetMap, SkeletonAsset,
    SkeletonBoneRecord, SkeletonIdentity, SkeletonRecord,
};
use engine_authoring::id::AssetId;
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Skeleton bind report (ADR 0077 §6): Problems panel "anim.skeleton_rebind"
// entry opens a detail view, per docs/ANIMATION_PIPELINE_PLAN.md AP-5.
// ---------------------------------------------------------------------------

/// Per-bone outcome of one reimport, relative to the previous
/// [`SkeletonRecord`] generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkeletonBoneStatus {
    /// Present (by name) in both the previous and current bone ledger.
    Kept,
    /// Present only in the current ledger: a new bone allocated this import.
    Added,
    /// Present only in the previous ledger: retired, its `BoneId` never reused.
    Retired,
}

/// One row of a [`SkeletonBindReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkeletonBindReportRow {
    /// Bone name (current name for Kept/Added, last known name for Retired).
    pub name: String,
    /// The bone's stable [`BoneId`] value.
    pub bone_id: u32,
    /// Outcome classification for this bone.
    pub status: SkeletonBoneStatus,
}

/// Per-bone bind report for one skeleton, comparing two
/// [`SkeletonRecord`] generations (ADR 0077 §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkeletonBindReport {
    /// Sub-asset ID string of the skeleton this report describes.
    pub skeleton_id: String,
    /// One row per bone touched by the reimport, current bones first
    /// (Kept/Added, in current ledger order), then Retired bones.
    pub rows: Vec<SkeletonBindReportRow>,
}

impl SkeletonBindReport {
    /// Number of bones classified as [`SkeletonBoneStatus::Kept`].
    pub fn kept_count(&self) -> usize {
        self.count(SkeletonBoneStatus::Kept)
    }

    /// Number of bones classified as [`SkeletonBoneStatus::Added`].
    pub fn added_count(&self) -> usize {
        self.count(SkeletonBoneStatus::Added)
    }

    /// Number of bones classified as [`SkeletonBoneStatus::Retired`].
    pub fn retired_count(&self) -> usize {
        self.count(SkeletonBoneStatus::Retired)
    }

    fn count(&self, status: SkeletonBoneStatus) -> usize {
        self.rows.iter().filter(|row| row.status == status).count()
    }
}

/// Builds a [`SkeletonBindReport`] by comparing the bone name ledgers of two
/// [`SkeletonRecord`] generations (ADR 0077 §6 matches
/// by exact bone name; this mirrors that rule for display purposes only —
/// it never reassigns a `BoneId`).
///
/// A bone name present in both ledgers is [`SkeletonBoneStatus::Kept`]
/// (reported with its *current* `bone_id`, which reimport matching keeps
/// stable); a name present only in `current_bones` is
/// [`SkeletonBoneStatus::Added`]; a name present only in `previous_bones` is
/// [`SkeletonBoneStatus::Retired`].
pub fn build_skeleton_bind_report(
    skeleton_id: &str,
    previous_bones: &[SkeletonBoneRecord],
    current_bones: &[SkeletonBoneRecord],
) -> SkeletonBindReport {
    let mut rows = Vec::with_capacity(current_bones.len() + previous_bones.len());
    for bone in current_bones {
        let status = if previous_bones
            .iter()
            .any(|previous| previous.name == bone.name)
        {
            SkeletonBoneStatus::Kept
        } else {
            SkeletonBoneStatus::Added
        };
        rows.push(SkeletonBindReportRow {
            name: bone.name.clone(),
            bone_id: bone.bone_id,
            status,
        });
    }
    for bone in previous_bones {
        if !current_bones
            .iter()
            .any(|current| current.name == bone.name)
        {
            rows.push(SkeletonBindReportRow {
                name: bone.name.clone(),
                bone_id: bone.bone_id,
                status: SkeletonBoneStatus::Retired,
            });
        }
    }
    SkeletonBindReport {
        skeleton_id: skeleton_id.to_owned(),
        rows,
    }
}

// ---------------------------------------------------------------------------
// RetargetMap inspector (ADR 0079 §1)
// ---------------------------------------------------------------------------

/// One [`engine::BonePair`] with both bone IDs resolved to display names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBonePair {
    /// Human-readable source bone name.
    pub source_name: String,
    /// Human-readable target bone name.
    pub target_name: String,
}

/// One [`engine::ChainPair`] with every bone ID resolved to a display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedChainPair {
    /// Diagnostic label carried by the chain pair.
    pub name: String,
    /// Root-to-tip source bone names.
    pub source_names: Vec<String>,
    /// Root-to-tip target bone names.
    pub target_names: Vec<String>,
}

/// A target skeleton bone that appears in neither `bone_pairs` nor
/// `chain_pairs` of an inspected [`RetargetMap`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedTargetBone {
    /// The bone's stable [`BoneId`] value.
    pub bone_id: u32,
    /// Bone name.
    pub name: String,
}

/// State -> widget model for the RetargetMap inspector (AP-5).
#[derive(Debug, Clone, PartialEq)]
pub struct RetargetMapInspectorModel {
    /// Source skeleton sub-asset ID string.
    pub source_skeleton_id: String,
    /// Source skeleton display name, or the ID string when unresolved.
    pub source_skeleton_name: String,
    /// Target skeleton sub-asset ID string.
    pub target_skeleton_id: String,
    /// Target skeleton display name, or the ID string when unresolved.
    pub target_skeleton_name: String,
    /// Mirrors [`RetargetMap::always_package`] (AP-7): forces packaging to
    /// bake this map even when the reachability trace finds no scene or
    /// prefab content that needs its skeleton pair.
    pub always_package: bool,
    /// `true` when [`RetargetMap::validate`] reports
    /// [`engine::RETARGET_MAP_STALE_DIAGNOSTIC`] against the manifest's
    /// current skeleton identities. `false` when either skeleton's ledger is
    /// not currently resolvable (never claims staleness it cannot verify).
    pub stale: bool,
    /// Resolved direct bone-to-bone mappings.
    pub bone_pairs: Vec<ResolvedBonePair>,
    /// Resolved chain mappings.
    pub chain_pairs: Vec<ResolvedChainPair>,
    /// Target skeleton bones mapped by neither `bone_pairs` nor `chain_pairs`.
    pub unresolved_target_bones: Vec<UnresolvedTargetBone>,
}

/// Finds the [`engine::SkeletonRecord`] for `skeleton_id` among every
/// registered source's `ImportSettings::skeleton_records`.
///
/// A skeleton's ledger may be recorded under a *different* source than the
/// one it is conceptually "about" when the ADR 0077 §4 dedupe rule adopted
/// it, so every manifest entry is searched rather than just the skeleton's
/// own naturally-derived source.
pub fn find_skeleton_record<'a>(
    manifest: &'a engine::AssetManifest,
    skeleton_id: &str,
) -> Option<&'a engine::SkeletonRecord> {
    manifest.iter().find_map(|(_, entry)| {
        entry
            .import_settings
            .skeleton_records
            .iter()
            .find(|record| record.id == skeleton_id)
    })
}

/// Finds the human-readable display name for a skeleton sub-asset, searching
/// every registered source's `ImportSettings::sub_assets` catalog.
pub fn find_skeleton_display_name(
    manifest: &engine::AssetManifest,
    skeleton_id: &str,
) -> Option<String> {
    manifest.iter().find_map(|(_, entry)| {
        entry
            .import_settings
            .sub_assets
            .iter()
            .find(|sub_asset| {
                sub_asset.id == skeleton_id && sub_asset.kind == ImportedSubAssetKind::Skeleton
            })
            .map(|sub_asset| sub_asset.name.clone())
    })
}

/// Resolves a [`BoneId`] to its recorded name, falling back to a `bone#N`
/// placeholder when the ledger does not (or no longer) contains it.
pub fn resolve_bone_name(bones: &[SkeletonBoneRecord], id: BoneId) -> String {
    bones
        .iter()
        .find(|bone| bone.bone_id == id.0)
        .map(|bone| bone.name.clone())
        .unwrap_or_else(|| format!("bone#{}", id.0))
}

/// Builds the RetargetMap inspector's state -> widget model (AP-5).
pub fn build_retarget_map_inspector_model(
    map: &RetargetMap,
    manifest: &engine::AssetManifest,
) -> RetargetMapInspectorModel {
    let source_record = find_skeleton_record(manifest, map.source_skeleton.as_str());
    let target_record = find_skeleton_record(manifest, map.target_skeleton.as_str());
    let empty: Vec<SkeletonBoneRecord> = Vec::new();
    let source_bones = source_record.map_or(empty.as_slice(), |record| record.bones.as_slice());
    let target_bones = target_record.map_or(empty.as_slice(), |record| record.bones.as_slice());

    let stale = match (source_record, target_record) {
        (Some(source_record), Some(target_record)) => !map
            .validate(
                SkeletonIdentity(source_record.identity),
                SkeletonIdentity(target_record.identity),
            )
            .is_empty(),
        _ => false,
    };

    let bone_pairs = map
        .bone_pairs
        .iter()
        .map(|pair| ResolvedBonePair {
            source_name: resolve_bone_name(source_bones, pair.source),
            target_name: resolve_bone_name(target_bones, pair.target),
        })
        .collect();
    let chain_pairs = map
        .chain_pairs
        .iter()
        .map(|chain| ResolvedChainPair {
            name: chain.name.clone(),
            source_names: chain
                .source_bones
                .iter()
                .map(|id| resolve_bone_name(source_bones, *id))
                .collect(),
            target_names: chain
                .target_bones
                .iter()
                .map(|id| resolve_bone_name(target_bones, *id))
                .collect(),
        })
        .collect();

    let mapped_targets: BTreeSet<u32> = map
        .bone_pairs
        .iter()
        .map(|pair| pair.target.0)
        .chain(
            map.chain_pairs
                .iter()
                .flat_map(|chain| chain.target_bones.iter().map(|id| id.0)),
        )
        .collect();
    let unresolved_target_bones = target_bones
        .iter()
        .filter(|bone| !mapped_targets.contains(&bone.bone_id))
        .map(|bone| UnresolvedTargetBone {
            bone_id: bone.bone_id,
            name: bone.name.clone(),
        })
        .collect();

    RetargetMapInspectorModel {
        source_skeleton_id: map.source_skeleton.as_str().to_owned(),
        source_skeleton_name: find_skeleton_display_name(manifest, map.source_skeleton.as_str())
            .unwrap_or_else(|| map.source_skeleton.as_str().to_owned()),
        target_skeleton_id: map.target_skeleton.as_str().to_owned(),
        target_skeleton_name: find_skeleton_display_name(manifest, map.target_skeleton.as_str())
            .unwrap_or_else(|| map.target_skeleton.as_str().to_owned()),
        always_package: map.always_package,
        stale,
        bone_pairs,
        chain_pairs,
        unresolved_target_bones,
    }
}

/// Builds a matching-only [`SkeletonAsset`] from a persisted bone ledger.
///
/// [`engine::generate_retarget_map`] reads only bone `id` and `name` (never
/// rest pose), so the rest pose here is an unused placeholder. This lets the
/// editor re-run the name-matching heuristic — and drive the retarget-map
/// creation flow — from data already in the manifest without persisting rest
/// TRS there. ADR 0077 §5 deliberately keeps rest pose out of the manifest so
/// neither action ever needs a full re-import.
pub fn synthetic_skeleton_asset(
    id: AssetId,
    identity: u64,
    bones: &[SkeletonBoneRecord],
) -> SkeletonAsset {
    let next_bone_id = bones.iter().map(|bone| bone.bone_id + 1).max().unwrap_or(0);
    SkeletonAsset {
        id,
        name: String::new(),
        bones: bones
            .iter()
            .map(|bone| BoneDef {
                id: BoneId(bone.bone_id),
                name: bone.name.clone(),
                parent: None,
                rest_translation: Vec3::ZERO,
                rest_rotation: Quat::IDENTITY,
                rest_scale: Vec3::ONE,
            })
            .collect(),
        identity: SkeletonIdentity(identity),
        next_bone_id,
    }
}

/// Merges re-run name-matching results into `existing`.
///
/// Every existing pair wins outright; a newly generated pair is added only
/// when its target bone is not already covered by an existing pair (ADR 0079
/// "the heuristic runs once... the asset stores only explicit results",
/// applied here to the inspector's re-run action rather than first creation).
pub fn merge_bone_pairs(existing: &[BonePair], generated: &[BonePair]) -> Vec<BonePair> {
    let mut merged = existing.to_vec();
    let covered: BTreeSet<u32> = existing.iter().map(|pair| pair.target.0).collect();
    for pair in generated {
        if !covered.contains(&pair.target.0) {
            merged.push(*pair);
        }
    }
    merged
}

/// Re-runs [`engine::generate_retarget_map`]'s name-matching heuristic
/// against `map`'s own recorded skeletons and merges the result into `map`'s
/// existing `bone_pairs` via [`merge_bone_pairs`] (AP-5 "Re-run name
/// matching" action). Every other field of `map` (schema version, skeleton
/// references and identities, `chain_pairs`, `translation`) is unchanged.
pub fn rerun_name_matching(
    map: &RetargetMap,
    source_bones: &[SkeletonBoneRecord],
    target_bones: &[SkeletonBoneRecord],
) -> RetargetMap {
    let source_asset = synthetic_skeleton_asset(
        map.source_skeleton.clone(),
        map.source_identity,
        source_bones,
    );
    let target_asset = synthetic_skeleton_asset(
        map.target_skeleton.clone(),
        map.target_identity,
        target_bones,
    );
    let generated = engine::generate_retarget_map(&source_asset, &target_asset);
    RetargetMap {
        bone_pairs: merge_bone_pairs(&map.bone_pairs, &generated.bone_pairs),
        ..map.clone()
    }
}

// ---------------------------------------------------------------------------
// Retarget map creation flow (AP-5, `anim.retarget_map_missing`)
// ---------------------------------------------------------------------------

/// Builds the `<source-stem>_to_<target-stem>.retarget.json` file name used
/// by the creation-flow action.
pub fn retarget_map_file_name(source_stem: &str, target_stem: &str) -> String {
    format!("{source_stem}_to_{target_stem}.retarget.json")
}

// ---------------------------------------------------------------------------
// Multi-skin retarget map creation picker (AP-6 scope (b))
// ---------------------------------------------------------------------------

/// One selectable row in the multi-skin retarget-map creation picker: a
/// single [`SkeletonRecord`] presented as `"<skin name> (<bone count> bones)"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetargetMapPickerRow {
    /// Rendered row label.
    pub label: String,
}

/// State -> widget model for the picker `create_retarget_map_from_browser`
/// opens when either side of a (source, target) pair records more than one
/// skeleton (multiple skins in one imported file, AP-6 scope (b)). Exactly
/// one record on both sides means the caller never builds this model at all
/// and keeps the one-click creation path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetargetMapCreationPickerModel {
    /// One row per entry of the source side's `skeleton_records`, same order
    /// and index as the slice the caller resolves selections against.
    pub source_rows: Vec<RetargetMapPickerRow>,
    /// One row per entry of the target side's `skeleton_records`, same order
    /// and index as the slice the caller resolves selections against.
    pub target_rows: Vec<RetargetMapPickerRow>,
}

/// Builds one picker row's label for a recorded skeleton.
fn build_retarget_map_picker_row(
    manifest: &engine::AssetManifest,
    record: &SkeletonRecord,
) -> RetargetMapPickerRow {
    let name =
        find_skeleton_display_name(manifest, &record.id).unwrap_or_else(|| record.id.clone());
    RetargetMapPickerRow {
        label: format!("{name} ({} bones)", record.bones.len()),
    }
}

/// Builds the multi-skin retarget-map creation picker's state -> widget
/// model (AP-6 scope (b)).
pub fn build_retarget_map_creation_picker_model(
    manifest: &engine::AssetManifest,
    source_records: &[SkeletonRecord],
    target_records: &[SkeletonRecord],
) -> RetargetMapCreationPickerModel {
    RetargetMapCreationPickerModel {
        source_rows: source_records
            .iter()
            .map(|record| build_retarget_map_picker_row(manifest, record))
            .collect(),
        target_rows: target_records
            .iter()
            .map(|record| build_retarget_map_picker_row(manifest, record))
            .collect(),
    }
}

/// Action requested by the multi-skin retarget-map creation picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetargetMapCreationPickerAction {
    /// The user confirmed the currently selected (source, target) pair;
    /// the caller generates and writes the map for that pair.
    Confirm,
    /// The user canceled; no map file is written.
    Cancel,
}

/// Renders the multi-skin retarget-map creation picker (AP-6 scope (b)).
///
/// `selected_source` / `selected_target` are indices into
/// `model.source_rows` / `model.target_rows`; the caller owns them as
/// persistent UI state (they survive across frames until Confirm or Cancel)
/// and reads them back to resolve the chosen `SkeletonRecord`s after a
/// [`RetargetMapCreationPickerAction::Confirm`].
pub fn show_retarget_map_creation_picker(
    ui: &mut egui::Ui,
    model: &RetargetMapCreationPickerModel,
    selected_source: &mut usize,
    selected_target: &mut usize,
) -> Option<RetargetMapCreationPickerAction> {
    let mut action = None;
    ui.label(
        "The source or target records more than one skeleton. Choose which \
         pair this retarget map should map:",
    );
    ui.separator();
    ui.strong("Source skeleton");
    for (index, row) in model.source_rows.iter().enumerate() {
        ui.radio_value(selected_source, index, &row.label);
    }
    ui.separator();
    ui.strong("Target skeleton");
    for (index, row) in model.target_rows.iter().enumerate() {
        ui.radio_value(selected_target, index, &row.label);
    }
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Create map").clicked() {
            action = Some(RetargetMapCreationPickerAction::Confirm);
        }
        if ui.button("Cancel").clicked() {
            action = Some(RetargetMapCreationPickerAction::Cancel);
        }
    });
    action
}

// ---------------------------------------------------------------------------
// egui rendering (state -> widget model consumed here; kept thin per the
// module's split, mirroring crate::material_editor)
// ---------------------------------------------------------------------------

/// Renders a [`SkeletonBindReport`] as a kept/added/retired bone table.
pub fn show_skeleton_bind_report(ui: &mut egui::Ui, report: &SkeletonBindReport) {
    ui.label(format!(
        "{} kept, {} added, {} retired",
        report.kept_count(),
        report.added_count(),
        report.retired_count()
    ));
    egui::Grid::new(("skeleton_bind_report", &report.skeleton_id))
        .striped(true)
        .show(ui, |ui| {
            ui.strong("Bone");
            ui.strong("BoneId");
            ui.strong("Status");
            ui.end_row();
            for row in &report.rows {
                ui.label(&row.name);
                ui.label(row.bone_id.to_string());
                let (text, color) = match row.status {
                    SkeletonBoneStatus::Kept => ("kept", egui::Color32::LIGHT_GRAY),
                    SkeletonBoneStatus::Added => ("added", egui::Color32::from_rgb(120, 200, 120)),
                    SkeletonBoneStatus::Retired => {
                        ("retired", egui::Color32::from_rgb(220, 140, 60))
                    }
                };
                ui.colored_label(color, text);
                ui.end_row();
            }
        });
}

/// Action requested by the RetargetMap inspector panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetargetMapInspectorAction {
    /// The user clicked "Re-run name matching".
    RerunNameMatching,
    /// The user toggled "Always package" to the carried value (AP-7); the
    /// caller writes [`RetargetMap::always_package`] back to the file.
    SetAlwaysPackage(bool),
}

/// Renders a [`RetargetMapInspectorModel`], returning the action the user
/// requested, if any.
pub fn show_retarget_map_inspector(
    ui: &mut egui::Ui,
    model: &RetargetMapInspectorModel,
) -> Option<RetargetMapInspectorAction> {
    let mut action = None;
    ui.label(format!(
        "Source: {} ({})",
        model.source_skeleton_name, model.source_skeleton_id
    ));
    ui.label(format!(
        "Target: {} ({})",
        model.target_skeleton_name, model.target_skeleton_id
    ));
    if model.stale {
        ui.colored_label(
            egui::Color32::from_rgb(220, 60, 60),
            "Stale: a referenced skeleton was reimported and its structure changed. \
             Review bone_pairs/chain_pairs before using this map.",
        );
    }
    if ui.button("Re-run name matching").clicked() {
        action = Some(RetargetMapInspectorAction::RerunNameMatching);
    }
    let mut always_package = model.always_package;
    if ui
        .checkbox(&mut always_package, "Always package")
        .on_hover_text(
            "Bake and stage this map into every build even if no scanned scene or prefab \
             needs its skeleton pair (for clips assigned dynamically at runtime).",
        )
        .changed()
    {
        action = Some(RetargetMapInspectorAction::SetAlwaysPackage(always_package));
    }
    ui.separator();
    ui.strong(format!("Bone pairs ({})", model.bone_pairs.len()));
    egui::Grid::new("retarget_map_bone_pairs")
        .striped(true)
        .show(ui, |ui| {
            ui.strong("Source bone");
            ui.strong("Target bone");
            ui.end_row();
            for pair in &model.bone_pairs {
                ui.label(&pair.source_name);
                ui.label(&pair.target_name);
                ui.end_row();
            }
        });
    if !model.chain_pairs.is_empty() {
        ui.separator();
        ui.strong(format!("Chain pairs ({})", model.chain_pairs.len()));
        for chain in &model.chain_pairs {
            ui.label(format!(
                "{}: [{}] -> [{}]",
                chain.name,
                chain.source_names.join(", "),
                chain.target_names.join(", ")
            ));
        }
    }
    ui.separator();
    if model.unresolved_target_bones.is_empty() {
        ui.label("No unresolved target bones.");
    } else {
        ui.colored_label(
            egui::Color32::from_rgb(220, 180, 60),
            format!(
                "Unresolved target bones ({})",
                model.unresolved_target_bones.len()
            ),
        );
        for bone in &model.unresolved_target_bones {
            ui.label(format!("  {} (#{})", bone.name, bone.bone_id));
        }
    }
    action
}

/// Renders a string-list editor for `ImportSettings::contact_bones`
/// (ADR 0080 §1 override). Returns `true` when the list changed.
pub fn show_contact_bones_editor(ui: &mut egui::Ui, contact_bones: &mut Vec<String>) -> bool {
    let mut changed = false;
    ui.label(
        "Contact bones override (replaces the default foot/ankle/toe name \
         heuristic when non-empty):",
    );
    let mut removed = None;
    for (index, name) in contact_bones.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            if ui.text_edit_singleline(name).changed() {
                changed = true;
            }
            if ui.small_button("Remove").clicked() {
                removed = Some(index);
            }
        });
    }
    if let Some(index) = removed {
        contact_bones.remove(index);
        changed = true;
    }
    if ui.button("Add bone name").clicked() {
        contact_bones.push(String::new());
        changed = true;
    }
    changed
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use engine::TranslationPolicy;

    fn bone_record(bone_id: u32, name: &str) -> SkeletonBoneRecord {
        SkeletonBoneRecord {
            bone_id,
            name: name.to_owned(),
        }
    }

    // --- build_skeleton_bind_report -----------------------------------------

    #[test]
    fn build_skeleton_bind_report_classifies_kept_added_and_retired_bones() {
        let previous = vec![bone_record(0, "root"), bone_record(1, "old_tip")];
        let current = vec![bone_record(0, "root"), bone_record(2, "new_tip")];

        let report = build_skeleton_bind_report("skeleton_1", &previous, &current);

        assert_eq!(report.kept_count(), 1);
        assert_eq!(report.added_count(), 1);
        assert_eq!(report.retired_count(), 1);
        assert!(report
            .rows
            .iter()
            .any(|row| row.name == "root" && row.status == SkeletonBoneStatus::Kept));
        assert!(report
            .rows
            .iter()
            .any(|row| row.name == "new_tip" && row.status == SkeletonBoneStatus::Added));
        assert!(report
            .rows
            .iter()
            .any(|row| row.name == "old_tip" && row.status == SkeletonBoneStatus::Retired));
    }

    #[test]
    fn build_skeleton_bind_report_on_first_import_marks_every_bone_added() {
        let current = vec![bone_record(0, "root"), bone_record(1, "tip")];

        let report = build_skeleton_bind_report("skeleton_1", &[], &current);

        assert_eq!(report.added_count(), 2);
        assert_eq!(report.kept_count(), 0);
        assert_eq!(report.retired_count(), 0);
    }

    #[test]
    fn build_skeleton_bind_report_with_no_changes_is_entirely_kept() {
        let bones = vec![bone_record(0, "root"), bone_record(1, "tip")];

        let report = build_skeleton_bind_report("skeleton_1", &bones, &bones);

        assert_eq!(report.kept_count(), 2);
        assert_eq!(report.added_count(), 0);
        assert_eq!(report.retired_count(), 0);
    }

    // --- RetargetMap inspector model -----------------------------------------

    fn sample_map(source: AssetId, target: AssetId) -> RetargetMap {
        RetargetMap {
            schema_version: engine::RETARGET_MAP_SCHEMA_VERSION,
            source_skeleton: source,
            source_identity: 1,
            target_skeleton: target,
            target_identity: 2,
            bone_pairs: vec![BonePair {
                source: BoneId(0),
                target: BoneId(0),
            }],
            chain_pairs: Vec::new(),
            translation: TranslationPolicy::default(),
            always_package: false,
        }
    }

    fn manifest_with_skeleton(
        manifest: &mut engine::AssetManifest,
        source_id: AssetId,
        skeleton_id: &str,
        skeleton_name: &str,
        identity: u64,
        bones: Vec<SkeletonBoneRecord>,
    ) {
        manifest.insert(
            source_id,
            engine::ManifestEntry {
                path: format!("{skeleton_name}.gltf"),
                name: Some(skeleton_name.to_owned()),
                import_settings: engine::ImportSettings {
                    sub_assets: vec![engine::ImportedSubAsset {
                        id: skeleton_id.to_owned(),
                        kind: ImportedSubAssetKind::Skeleton,
                        name: format!("{skeleton_name} Skeleton"),
                        index: 0,
                        target_model_source: None,
                    }],
                    skeleton_records: vec![engine::SkeletonRecord {
                        id: skeleton_id.to_owned(),
                        identity,
                        next_bone_id: bones.len() as u32,
                        bones,
                    }],
                    ..engine::ImportSettings::default()
                },
            },
        );
    }

    #[test]
    fn build_retarget_map_inspector_model_lists_unresolved_target_bones() {
        let source_asset_id = AssetId::generate();
        let target_asset_id = AssetId::generate();
        let source_skeleton = AssetId::generate();
        let target_skeleton = AssetId::generate();
        let mut manifest = engine::AssetManifest::default();
        manifest_with_skeleton(
            &mut manifest,
            source_asset_id,
            source_skeleton.as_str(),
            "hero",
            1,
            vec![bone_record(0, "root")],
        );
        manifest_with_skeleton(
            &mut manifest,
            target_asset_id,
            target_skeleton.as_str(),
            "villain",
            2,
            vec![bone_record(0, "root"), bone_record(1, "extra_tail")],
        );
        let map = sample_map(source_skeleton, target_skeleton);

        let model = build_retarget_map_inspector_model(&map, &manifest);

        assert_eq!(model.bone_pairs.len(), 1);
        assert_eq!(model.bone_pairs[0].source_name, "root");
        assert_eq!(model.bone_pairs[0].target_name, "root");
        assert_eq!(model.unresolved_target_bones.len(), 1);
        assert_eq!(model.unresolved_target_bones[0].name, "extra_tail");
        assert!(!model.stale, "identities match the recorded ones");
    }

    #[test]
    fn build_retarget_map_inspector_model_flags_stale_when_identity_changed() {
        let source_asset_id = AssetId::generate();
        let target_asset_id = AssetId::generate();
        let source_skeleton = AssetId::generate();
        let target_skeleton = AssetId::generate();
        let mut manifest = engine::AssetManifest::default();
        manifest_with_skeleton(
            &mut manifest,
            source_asset_id,
            source_skeleton.as_str(),
            "hero",
            1,
            vec![bone_record(0, "root")],
        );
        // Recorded identity (2) no longer matches the manifest's current
        // identity (99) for this skeleton, simulating a reimport that
        // changed the rig after the map was authored.
        manifest_with_skeleton(
            &mut manifest,
            target_asset_id,
            target_skeleton.as_str(),
            "villain",
            99,
            vec![bone_record(0, "root")],
        );
        let map = sample_map(source_skeleton, target_skeleton);

        let model = build_retarget_map_inspector_model(&map, &manifest);

        assert!(model.stale, "target identity drifted from the map's record");
    }

    #[test]
    fn build_retarget_map_inspector_model_carries_always_package() {
        let manifest = engine::AssetManifest::default();
        let mut map = sample_map(AssetId::generate(), AssetId::generate());
        map.always_package = true;

        let model = build_retarget_map_inspector_model(&map, &manifest);

        assert!(
            model.always_package,
            "the model must mirror RetargetMap::always_package"
        );
    }

    #[test]
    fn build_retarget_map_inspector_model_does_not_claim_staleness_when_unresolvable() {
        // Neither skeleton is registered in the manifest at all: the model
        // must not claim staleness it cannot verify.
        let manifest = engine::AssetManifest::default();
        let map = sample_map(AssetId::generate(), AssetId::generate());

        let model = build_retarget_map_inspector_model(&map, &manifest);

        assert!(!model.stale);
        assert!(model.bone_pairs[0].source_name.starts_with("bone#"));
    }

    // --- merge_bone_pairs / rerun_name_matching -------------------------------

    #[test]
    fn merge_bone_pairs_keeps_existing_pairs_over_generated_ones_for_the_same_target() {
        let existing = vec![BonePair {
            source: BoneId(5),
            target: BoneId(0),
        }];
        // A generated pair disagreeing about bone 0's source must lose.
        let generated = vec![
            BonePair {
                source: BoneId(9),
                target: BoneId(0),
            },
            BonePair {
                source: BoneId(1),
                target: BoneId(1),
            },
        ];

        let merged = merge_bone_pairs(&existing, &generated);

        assert_eq!(merged.len(), 2);
        assert!(merged.contains(&BonePair {
            source: BoneId(5),
            target: BoneId(0)
        }));
        assert!(merged.contains(&BonePair {
            source: BoneId(1),
            target: BoneId(1)
        }));
    }

    #[test]
    fn rerun_name_matching_fills_gaps_without_disturbing_explicit_pairs() {
        let source = AssetId::generate();
        let target = AssetId::generate();
        let mut map = sample_map(source.clone(), target.clone());
        // An explicit (and deliberately "wrong" by name) pair for bone 1,
        // which the name heuristic would otherwise map differently.
        map.bone_pairs = vec![
            BonePair {
                source: BoneId(0),
                target: BoneId(0),
            },
            BonePair {
                source: BoneId(9),
                target: BoneId(1),
            },
        ];
        let source_bones = vec![
            bone_record(0, "root"),
            bone_record(9, "tip"),
            bone_record(2, "extra"),
        ];
        let target_bones = vec![
            bone_record(0, "root"),
            bone_record(1, "tip"),
            bone_record(3, "extra"),
        ];

        let rerun = rerun_name_matching(&map, &source_bones, &target_bones);

        // Explicit pair for target bone 1 must survive unchanged even though
        // the name heuristic would have matched source bone 9 ("tip") to it
        // anyway in this case; the point under test is that an existing pair
        // is never replaced by a generated one.
        assert!(rerun.bone_pairs.contains(&BonePair {
            source: BoneId(9),
            target: BoneId(1)
        }));
        // The newly matched "extra" pair must fill the previously-unmapped gap.
        assert!(rerun.bone_pairs.contains(&BonePair {
            source: BoneId(2),
            target: BoneId(3)
        }));
        assert_eq!(rerun.schema_version, map.schema_version);
        assert_eq!(rerun.source_skeleton, map.source_skeleton);
        assert_eq!(rerun.target_skeleton, map.target_skeleton);
    }

    // --- retarget_map_file_name -----------------------------------------------

    #[test]
    fn retarget_map_file_name_follows_source_to_target_convention() {
        assert_eq!(
            retarget_map_file_name("hero", "mannequin"),
            "hero_to_mannequin.retarget.json"
        );
    }

    // --- build_retarget_map_creation_picker_model -----------------------------

    /// Registers one multi-skin source with two named skeletons, so its
    /// `skeleton_records` (and matching `sub_assets` catalog, for display
    /// name resolution) has more than one entry.
    fn manifest_with_two_skeletons(
        manifest: &mut engine::AssetManifest,
        source_id: AssetId,
        first: (&str, &str, usize),
        second: (&str, &str, usize),
    ) {
        let make_record = |id: &str, bone_count: usize| engine::SkeletonRecord {
            id: id.to_owned(),
            identity: 1,
            next_bone_id: bone_count as u32,
            bones: (0..bone_count as u32)
                .map(|bone_id| bone_record(bone_id, &format!("bone{bone_id}")))
                .collect(),
        };
        manifest.insert(
            source_id,
            engine::ManifestEntry {
                path: "rig.gltf".into(),
                name: Some("rig".into()),
                import_settings: engine::ImportSettings {
                    sub_assets: vec![
                        engine::ImportedSubAsset {
                            id: first.0.to_owned(),
                            kind: ImportedSubAssetKind::Skeleton,
                            name: first.1.to_owned(),
                            index: 0,
                            target_model_source: None,
                        },
                        engine::ImportedSubAsset {
                            id: second.0.to_owned(),
                            kind: ImportedSubAssetKind::Skeleton,
                            name: second.1.to_owned(),
                            index: 1,
                            target_model_source: None,
                        },
                    ],
                    skeleton_records: vec![
                        make_record(first.0, first.2),
                        make_record(second.0, second.2),
                    ],
                    ..engine::ImportSettings::default()
                },
            },
        );
    }

    #[test]
    fn build_retarget_map_creation_picker_model_labels_rows_with_name_and_bone_count() {
        let source_id = AssetId::generate();
        let mut manifest = engine::AssetManifest::default();
        manifest_with_two_skeletons(
            &mut manifest,
            source_id.clone(),
            ("skel_a", "Upper Body", 3),
            ("skel_b", "Lower Body", 5),
        );
        let source_records = manifest
            .get(&source_id)
            .expect("manifest entry")
            .import_settings
            .skeleton_records
            .clone();

        let model = build_retarget_map_creation_picker_model(&manifest, &source_records, &[]);

        assert_eq!(model.source_rows.len(), 2);
        assert_eq!(model.source_rows[0].label, "Upper Body (3 bones)");
        assert_eq!(model.source_rows[1].label, "Lower Body (5 bones)");
        assert!(model.target_rows.is_empty());
    }

    #[test]
    fn build_retarget_map_creation_picker_model_falls_back_to_id_when_name_unresolved() {
        let record = engine::SkeletonRecord {
            id: "unregistered_skeleton".into(),
            identity: 1,
            next_bone_id: 1,
            bones: vec![bone_record(0, "root")],
        };
        let manifest = engine::AssetManifest::default();

        let model = build_retarget_map_creation_picker_model(&manifest, &[record], &[]);

        assert_eq!(model.source_rows.len(), 1);
        assert_eq!(
            model.source_rows[0].label,
            "unregistered_skeleton (1 bones)"
        );
    }
}
