//! VMD motion pairing picker and VMD/PMX compatibility reporting.
//!
//! A VMD motion is baked against a specific PMX model, so this module shows
//! which models a motion targets and reports, on request, how well a given
//! model matches it.

use crate::ui::*;

/// The paired-model picker shown for a `*.vmd` motion source.
pub(in crate::ui) struct MotionPairingState {
    /// Absolute VMD path used only when the author requests a compatibility
    /// check. The check never modifies or reimports this file.
    pub(in crate::ui) motion_path: Option<PathBuf>,
    /// Optional PMX whose MMD IK and appended-parent rig is evaluated once.
    /// `None` is the ordinary direct-bake path, not an invalid state.
    pub(in crate::ui) original: Option<AssetId>,
    /// Editable PMX target list. Each selected model produces its own stable
    /// Animation Clip sub-asset.
    pub(in crate::ui) selected: Vec<AssetId>,
    /// Every registered `.pmx` in the project as `(id, display label)`, in
    /// stable manifest order so the list does not reshuffle between frames.
    pub(in crate::ui) candidates: Vec<(AssetId, String)>,
    /// Absolute PMX paths parallel to `candidates`, retained because the
    /// compatibility button parses current source bytes on demand.
    pub(in crate::ui) candidate_paths: Vec<(AssetId, PathBuf)>,
    /// Model-source pairs backed by a currently registered Retarget Map.
    /// Stored as source/target model IDs so the UI can report readiness
    /// without exposing internal skeleton sub-asset IDs.
    pub(in crate::ui) retarget_pairs: Vec<(AssetId, AssetId)>,
    /// Presentation-only model name stored in the VMD header. It helps the
    /// author choose an original PMX but is never used for automatic pairing.
    pub(in crate::ui) recorded_model_name: Option<String>,
    /// Transient reports from the latest explicit check. They are cleared
    /// whenever the original/output selection changes and are never saved.
    pub(in crate::ui) compatibility_reports: Vec<MotionCompatibilityDisplay>,
}

/// One PMX result shown in the VMD Import Settings compatibility section.
pub(in crate::ui) struct MotionCompatibilityDisplay {
    pub(in crate::ui) model_source: AssetId,
    pub(in crate::ui) result: Result<engine::VmdPmxCompatibilityReport, String>,
}

/// Renders the paired-model picker for a `*.vmd` motion source (ADR 0097 §3).
///
/// A VMD names bones but carries no rig, so it cannot be imported until the
/// author says which PMX to bake it against. The list is deliberately just
/// the project's `.pmx` sources — an FBX or glTF has no MMD IK/appended-parent
/// data to evaluate, so offering one would only produce a failed import.
pub(super) fn show_motion_pairing_editor(ui: &mut egui::Ui, pairing: &mut MotionPairingState) {
    ui.strong("Original PMX model (optional)");
    let previous_original = pairing.original.clone();
    let original_label = pairing
        .original
        .as_ref()
        .and_then(|selected| {
            pairing
                .candidates
                .iter()
                .find(|(id, _)| id == selected)
                .map(|(_, label)| label.as_str())
        })
        .unwrap_or("Not set - Direct bake");
    egui::ComboBox::from_id_salt("vmd_original_pmx")
        .selected_text(original_label)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut pairing.original, None, "Not set - Direct bake");
            for (id, label) in &pairing.candidates {
                ui.selectable_value(&mut pairing.original, Some(id.clone()), label);
            }
        });
    if pairing.original != previous_original {
        pairing.compatibility_reports.clear();
    }
    if let Some(recorded) = &pairing.recorded_model_name {
        ui.label(format!("VMD recorded model: {recorded}"));
    }
    if pairing.original.is_none() {
        ui.colored_label(
            egui::Color32::YELLOW,
            "Direct bake evaluates MMD constraints separately on each output PMX.",
        );
    } else {
        ui.label("The VMD is baked once on the original PMX, then retargeted to each output.");
    }
    ui.add_space(6.0);
    ui.strong("Output PMX models");
    ui.label("One target-specific clip is produced for each selected model.");
    if pairing.candidates.is_empty() {
        ui.label("No .pmx model is registered in this project yet.");
        return;
    }
    let mut output_selection_changed = false;
    for (id, label) in &pairing.candidates {
        let mut selected = pairing.selected.contains(id);
        if ui.checkbox(&mut selected, label).changed() {
            output_selection_changed = true;
            if selected {
                pairing.selected.push(id.clone());
            } else {
                pairing.selected.retain(|selected_id| selected_id != id);
            }
        }
    }
    if output_selection_changed {
        pairing.compatibility_reports.clear();
    }
    if !pairing.selected.is_empty() {
        ui.add_space(4.0);
        ui.strong("Processing summary");
        for target in &pairing.selected {
            let target_label = pairing
                .candidates
                .iter()
                .find(|(id, _)| id == target)
                .map(|(_, label)| label.as_str())
                .unwrap_or(target.as_str());
            let summary = match &pairing.original {
                None => format!("Direct bake -> {target_label}"),
                Some(original) if original == target => {
                    format!("Original bake -> {target_label} (same PMX)")
                }
                Some(original) => {
                    let original_label = pairing
                        .candidates
                        .iter()
                        .find(|(id, _)| id == original)
                        .map(|(_, label)| label.as_str())
                        .unwrap_or(original.as_str());
                    let status = if pairing
                        .retarget_pairs
                        .iter()
                        .any(|(source, output)| source == original && output == target)
                    {
                        "Ready"
                    } else {
                        "Missing"
                    };
                    format!(
                        "{original_label} -> Retarget Map ({status}) -> {target_label}"
                    )
                }
            };
            ui.label(summary);
        }
        if pairing.original.is_some() {
            ui.label("Missing maps must be created as explicit Retarget Map assets before reimport.");
        }
    }
    show_motion_compatibility_checker(ui, pairing);
}

/// Shows the opt-in name/operation checker separately from Save and Reimport.
/// Results describe current source bytes and deliberately remain transient.
fn show_motion_compatibility_checker(ui: &mut egui::Ui, pairing: &mut MotionPairingState) {
    ui.add_space(8.0);
    ui.separator();
    ui.strong("VMD / PMX compatibility check");
    ui.label(
        "Checks meaningful VMD bone and morph tracks by exact Japanese name. Neutral-only tracks are ignored.",
    );

    let has_check_target = pairing.original.is_some() || !pairing.selected.is_empty();
    let can_check = pairing.motion_path.is_some() && has_check_target;
    if ui
        .add_enabled(can_check, egui::Button::new("Check compatibility"))
        .on_hover_text(
            "Reads the current VMD and selected PMX files. This does not save, reimport, or modify assets.",
        )
        .clicked()
    {
        run_motion_compatibility_checks(pairing);
    }
    if !has_check_target {
        ui.label("Select an original or at least one output PMX to run the check.");
    } else if pairing.motion_path.is_none() {
        ui.colored_label(egui::Color32::RED, "The VMD source path is unavailable.");
    }

    for display in &pairing.compatibility_reports {
        ui.add_space(6.0);
        let label = pairing
            .candidates
            .iter()
            .find(|(id, _)| id == &display.model_source)
            .map(|(_, label)| label.as_str())
            .unwrap_or(display.model_source.as_str());
        ui.strong(label);
        match &display.result {
            Ok(report) => show_motion_compatibility_report(ui, pairing, display, report),
            Err(error) => {
                ui.colored_label(egui::Color32::RED, format!("Check failed: {error}"));
            }
        }
    }
}

/// Parses each distinct selected role in stable source/output order.
fn run_motion_compatibility_checks(pairing: &mut MotionPairingState) {
    pairing.compatibility_reports.clear();
    let Some(vmd_path) = pairing.motion_path.as_deref() else {
        return;
    };
    let mut targets = Vec::new();
    if let Some(original) = &pairing.original {
        targets.push(original.clone());
    }
    for output in &pairing.selected {
        if !targets.contains(output) {
            targets.push(output.clone());
        }
    }
    for target in targets {
        let result = pairing
            .candidate_paths
            .iter()
            .find(|(id, _)| id == &target)
            .ok_or_else(|| "The registered PMX source path is unavailable.".to_owned())
            .and_then(|(_, pmx_path)| {
                engine::check_vmd_pmx_compatibility_path(vmd_path, pmx_path)
                    .map_err(|error| error.to_string())
            });
        pairing.compatibility_reports.push(MotionCompatibilityDisplay {
            model_source: target,
            result,
        });
    }
}

/// Presents source and output roles without implying that direct output bone
/// names determine a Retarget Map conversion's success.
fn show_motion_compatibility_report(
    ui: &mut egui::Ui,
    pairing: &MotionPairingState,
    display: &MotionCompatibilityDisplay,
    report: &engine::VmdPmxCompatibilityReport,
) {
    let is_original = pairing.original.as_ref() == Some(&display.model_source);
    let is_retarget_output = pairing.original.is_some() && !is_original;
    if is_original {
        ui.label("Role: Original PMX (source bake rig)");
    } else {
        ui.label("Role: Output PMX");
    }
    let bone_label = if is_original {
        "Bone compatibility"
    } else if is_retarget_output {
        "Direct bone-name compatibility"
    } else {
        "Bone name compatibility"
    };
    show_compatibility_summary(ui, bone_label, report.bones);
    if is_retarget_output {
        ui.label("Informational only - bone conversion uses Retarget Map.");
    }
    show_compatibility_summary(ui, "Morph name compatibility", report.morphs);

    if report.issues.is_empty() {
        ui.colored_label(
            egui::Color32::GREEN,
            "No name ambiguity, missing track, or used-operation issue found.",
        );
        return;
    }
    let shown = report.issues.len().min(20);
    for issue in report.issues.iter().take(shown) {
        ui.colored_label(
            egui::Color32::YELLOW,
            format!(
                "{}: {} ({} keys)",
                compatibility_issue_label(issue.kind),
                issue.name,
                issue.keyframe_count
            ),
        );
    }
    if report.issues.len() > shown {
        ui.label(format!("... and {} more issues", report.issues.len() - shown));
    }
}

fn show_compatibility_summary(
    ui: &mut egui::Ui,
    label: &str,
    summary: engine::VmdPmxCompatibilitySummary,
) {
    match summary.compatibility_percent() {
        Some(percent) => ui.label(format!(
            "{label}: {percent:.1}% (unique {}/{}, missing {}, ambiguous {})",
            summary.unique_tracks,
            summary.used_tracks,
            summary.missing_tracks,
            summary.ambiguous_tracks
        )),
        None => ui.label(format!("{label}: N/A (no meaningful VMD tracks)")),
    };
}

fn compatibility_issue_label(kind: engine::VmdPmxCompatibilityIssueKind) -> &'static str {
    use engine::VmdPmxCompatibilityIssueKind as Kind;
    match kind {
        Kind::MissingBone => "Missing bone",
        Kind::AmbiguousBone => "Ambiguous bone name",
        Kind::RotationUnsupported => "Used rotation is not supported",
        Kind::TranslationUnsupported => "Used translation is not supported",
        Kind::MissingMorph => "Missing morph",
        Kind::AmbiguousMorph => "Ambiguous morph name",
    }
}
