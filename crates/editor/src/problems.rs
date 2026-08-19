//! Problems panel for persistent project-level validation diagnostics.

use eframe::egui;
use engine_authoring::{Diagnostic, DiagnosticTarget, Severity};
use std::collections::BTreeSet;

/// Severity switches used to narrow the Problems list without changing its
/// underlying diagnostics.
#[derive(Debug)]
struct SeverityFilter {
    /// Whether error diagnostics are visible.
    errors: bool,
    /// Whether warning diagnostics are visible.
    warnings: bool,
    /// Whether informational diagnostics are visible.
    info: bool,
}

impl Default for SeverityFilter {
    fn default() -> Self {
        Self {
            errors: true,
            warnings: true,
            // Informational import notices are available on demand but do not
            // compete with actionable diagnostics in the default view.
            info: false,
        }
    }
}

impl SeverityFilter {
    /// Returns whether `severity` is enabled by the current switches.
    fn accepts(&self, severity: Severity) -> bool {
        match severity {
            Severity::Error => self.errors,
            Severity::Warning => self.warnings,
            Severity::Info => self.info,
        }
    }
}

/// Result of drawing the Problems panel for one frame.
#[derive(Default)]
pub struct ProblemsOutput {
    /// Diagnostic clicked for editor navigation or a dedicated detail view.
    pub clicked: Option<Diagnostic>,
    /// Related semantic target selected from an explicit repair action.
    pub navigate_to: Option<DiagnosticTarget>,
    /// Whether the persistent code-suppression preference changed.
    pub suppression_changed: bool,
}

/// Panel that displays persistent project-level validation problems.
///
/// The panel owns only presentation state. Diagnostics remain owned by the
/// editor validation flow and are replaced through [`Self::set_problems`].
#[derive(Default)]
pub struct ProblemsPanel {
    /// Current set of problems produced by project validation.
    pub problems: Vec<Diagnostic>,
    /// Text used to limit the displayed and copied problems.
    ///
    /// An empty value displays every problem. Matching is case-insensitive and
    /// checks the same formatted code-and-message text shown in the panel.
    filter: String,
    /// Independent Error, Warning, and Info visibility switches.
    severity_filter: SeverityFilter,
    /// Warning and Info codes hidden from the persistent Problems surface.
    ///
    /// Errors deliberately ignore this set so a stale preference can never
    /// hide a newly blocking diagnostic.
    suppressed_codes: BTreeSet<String>,
}

impl ProblemsPanel {
    /// Replaces the current problem list.
    ///
    /// This preserves the presentation filter so repeated validation refreshes
    /// do not unexpectedly reset the user's current view.
    pub fn set_problems(&mut self, problems: Vec<Diagnostic>) {
        self.problems = problems;
    }

    /// Replaces the editor-local set of suppressed diagnostic codes.
    pub fn set_suppressed_codes(&mut self, codes: impl IntoIterator<Item = String>) {
        self.suppressed_codes = codes.into_iter().collect();
    }

    /// Returns suppressed codes in deterministic order for preference storage.
    pub fn suppressed_codes(&self) -> Vec<String> {
        self.suppressed_codes.iter().cloned().collect()
    }

    /// Returns whether `diagnostic` is hidden from Problems and status counts.
    ///
    /// Error diagnostics always remain active even if their code was stored by
    /// an older editor version or was previously used by a lower severity.
    pub fn is_suppressed(&self, diagnostic: &Diagnostic) -> bool {
        diagnostic.severity != Severity::Error
            && self.suppressed_codes.contains(&diagnostic.code)
    }

    /// Counts active, unsuppressed diagnostic groups at one severity.
    pub fn active_count(&self, severity: Severity) -> usize {
        self.grouped_count(severity, self.problems.iter())
    }

    /// Counts groups across Problems and another diagnostic collection.
    ///
    /// The editor uses this when producing global status totals because the
    /// Console may contain a mirrored copy of a persistent Problem. Grouping
    /// both collections together prevents that copy from being counted twice.
    pub fn active_count_with(&self, severity: Severity, additional: &[Diagnostic]) -> usize {
        self.grouped_count(severity, self.problems.iter().chain(additional))
    }

    /// Counts actionable Problems entries for the bottom-dock tab label.
    pub fn active_issue_count(&self) -> usize {
        self.active_count(Severity::Error) + self.active_count(Severity::Warning)
    }

    /// Highest active severity targeting an entity or one of its components.
    pub fn entity_severity(&self, entity: &engine_authoring::EntityId) -> Option<Severity> {
        self.problems
            .iter()
            .filter(|diagnostic| !self.is_suppressed(diagnostic))
            .filter(|diagnostic| match diagnostic.target.as_ref() {
                Some(DiagnosticTarget::Entity { id }) => id == entity,
                Some(DiagnosticTarget::Component { entity: id, .. }) => id == entity,
                _ => false,
            })
            .map(|diagnostic| diagnostic.severity)
            .max()
    }

    /// Draws the Problems panel into `ui`.
    ///
    /// Returns the full [`Diagnostic`] the user clicked, if any. Callers that
    /// only need navigation can read its target. Some diagnostic codes may
    /// additionally open a dedicated detail view.
    ///
    /// Copy All writes every currently visible row to the platform clipboard.
    /// It does not clear or otherwise mutate the underlying diagnostics.
    pub fn show(&mut self, ui: &mut egui::Ui) -> ProblemsOutput {
        let mut output = ProblemsOutput::default();
        ui.heading("Problems");

        // Counts exclude persistent suppressions but deliberately ignore the
        // temporary visibility switches, so each switch advertises how many
        // entries it would reveal.
        let error_count = self.active_count(Severity::Error);
        let warning_count = self.active_count(Severity::Warning);
        let info_count = self.active_count(Severity::Info);
        ui.horizontal(|ui| {
            ui.toggle_value(
                &mut self.severity_filter.errors,
                egui::RichText::new(format!("Errors ({error_count})"))
                    .color(egui::Color32::from_rgb(220, 60, 60)),
            );
            ui.toggle_value(
                &mut self.severity_filter.warnings,
                egui::RichText::new(format!("Warnings ({warning_count})"))
                    .color(egui::Color32::from_rgb(220, 180, 60)),
            );
            ui.toggle_value(
                &mut self.severity_filter.info,
                egui::RichText::new(format!("Info ({info_count})"))
                    .color(egui::Color32::LIGHT_GRAY),
            );
        });

        // Place Copy All beside the filter because both actions operate on the
        // currently visible subset. Recalculate after editing the filter so a
        // click in the same frame uses the latest entered text.
        ui.horizontal(|ui| {
            ui.label("Filter:");
            ui.text_edit_singleline(&mut self.filter);

            let copy_text = self.visible_copy_text();
            if ui
                .add_enabled(copy_text.is_some(), egui::Button::new("Copy All"))
                .on_hover_text("Copy every visible problem to the clipboard")
                .clicked()
                && let Some(copy_text) = copy_text
            {
                // egui forwards this text to the active platform clipboard
                // backend. Diagnostics remain unchanged after the copy.
                ui.ctx().copy_text(copy_text);
            }

            // Hidden codes remain discoverable and reversible even when no
            // current diagnostic uses them.
            let suppressed_codes = self.suppressed_codes();
            ui.menu_button(
                format!("Suppressed ({})", suppressed_codes.len()),
                |ui| {
                    if suppressed_codes.is_empty() {
                        ui.label("No suppressed diagnostic codes");
                    }
                    for code in suppressed_codes {
                        if ui.button(format!("Show {code}")).clicked() {
                            self.suppressed_codes.remove(&code);
                            output.suppression_changed = true;
                            ui.close();
                        }
                    }
                },
            );
        });

        // Generate the groups once for rendering. The same helper also supplies
        // Copy All, preventing display and clipboard filtering from diverging.
        let groups = self.visible_groups();

        egui::ScrollArea::vertical()
            .id_salt("problems_scroll")
            // The Problems body is the active content of a resizable bottom
            // dock, so it must claim the complete available height.
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if groups.is_empty() {
                    ui.label("No problems match the current filters");
                    return;
                }

                let mut suppress_code = None;
                for group in groups {
                    let color = severity_color(group.severity);

                    if group.diagnostics.len() == 1 {
                        let diagnostic = &group.diagnostics[0];
                        let response = ui.add(
                            egui::Label::new(
                                egui::RichText::new(problem_text(diagnostic)).color(color),
                            )
                            .wrap_mode(egui::TextWrapMode::Wrap)
                            .sense(egui::Sense::click()),
                        );

                        // Only diagnostics with a target are navigable. Copy
                        // All still includes diagnostics without a target.
                        if let Some(target) = &diagnostic.target {
                            let hover = target_hover_text(target);
                            if response.clicked() {
                                output.clicked = Some(diagnostic.clone());
                            }
                            response.clone().on_hover_text(hover);
                        }

                        add_suppression_menu(
                            &response,
                            group.severity,
                            &group.code,
                            &mut suppress_code,
                        );
                        if !diagnostic.related_targets.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.small("Repair:");
                                for target in &diagnostic.related_targets {
                                    if ui.small_button(repair_action_label(target)).clicked() {
                                        output.navigate_to = Some(target.clone());
                                    }
                                }
                            });
                        }
                        continue;
                    }

                    // Repeated diagnostics remain available under a collapsed
                    // summary. The grouping changes presentation only: each
                    // original diagnostic is retained for copy and navigation.
                    let group_id = format!(
                        "problems_group|{:?}|{}|{}",
                        group.severity,
                        group.code,
                        group
                            .target
                            .as_ref()
                            .map(target_hover_text)
                            .unwrap_or_default()
                    );
                    let collapsing = egui::CollapsingHeader::new(
                        egui::RichText::new(group.summary_text()).color(color),
                    )
                    .id_salt(group_id)
                    .default_open(false)
                    .show(ui, |ui| {
                        for diagnostic in &group.diagnostics {
                            let response = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&diagnostic.message).color(color),
                                )
                                .wrap_mode(egui::TextWrapMode::Wrap)
                                .sense(egui::Sense::click()),
                            );

                            if let Some(target) = &diagnostic.target {
                                if response.clicked() {
                                    output.clicked = Some(diagnostic.clone());
                                }
                                response.clone().on_hover_text(target_hover_text(target));
                            }
                            if !diagnostic.related_targets.is_empty() {
                                ui.horizontal_wrapped(|ui| {
                                    ui.small("Repair:");
                                    for target in &diagnostic.related_targets {
                                        if ui.small_button(repair_action_label(target)).clicked() {
                                            output.navigate_to = Some(target.clone());
                                        }
                                    }
                                });
                            }
                        }
                    });

                    if let Some(target) = &group.target {
                        collapsing
                            .header_response
                            .clone()
                            .on_hover_text(target_hover_text(target));
                    }
                    add_suppression_menu(
                        &collapsing.header_response,
                        group.severity,
                        &group.code,
                        &mut suppress_code,
                    );
                }

                // Defer the set mutation until row traversal is complete so
                // the current frame renders from one stable snapshot.
                if let Some(code) = suppress_code
                    && self.suppressed_codes.insert(code)
                {
                    output.suppression_changed = true;
                }
            });

        output
    }

    /// Produces the diagnostic groups matching the panel's current filter.
    ///
    /// Diagnostics group only when severity, stable code, and navigation target
    /// all match. This keeps unrelated assets and entities independently
    /// navigable while compacting repeated per-item importer notices.
    fn visible_groups(&self) -> Vec<ProblemGroup> {
        let filter = self.filter.to_ascii_lowercase();
        let mut groups = Vec::new();

        for diagnostic in &self.problems {
            if self.is_suppressed(diagnostic)
                || !self.severity_filter.accepts(diagnostic.severity)
            {
                continue;
            }

            let text = problem_text(diagnostic);

            // Filtering happens before grouping so an expanded group contains
            // only the child messages matching the current search text.
            if !filter.is_empty() && !text.to_ascii_lowercase().contains(&filter) {
                continue;
            }

            if let Some(group) = groups
                .iter_mut()
                .find(|group: &&mut ProblemGroup| group.matches(diagnostic))
            {
                group.diagnostics.push(diagnostic.clone());
            } else {
                groups.push(ProblemGroup::new(diagnostic.clone()));
            }
        }

        groups
    }

    /// Builds the newline-delimited clipboard payload for visible rows.
    ///
    /// Returns `None` when the current filter produces no rows. The caller uses
    /// this distinction to disable Copy All instead of copying an empty string.
    fn visible_copy_text(&self) -> Option<String> {
        let groups = self
            .visible_groups()
            .into_iter()
            .map(|group| group.copy_text())
            .collect::<Vec<_>>();

        if groups.is_empty() {
            None
        } else {
            Some(groups.join("\n"))
        }
    }

    /// Counts unique code-and-target groups at `severity`.
    ///
    /// Presentation filters are intentionally ignored because status totals
    /// describe every active issue, including groups temporarily hidden by a
    /// severity switch or search text.
    fn grouped_count<'a>(
        &self,
        severity: Severity,
        diagnostics: impl IntoIterator<Item = &'a Diagnostic>,
    ) -> usize {
        let mut groups: Vec<ProblemGroup> = Vec::new();

        for diagnostic in diagnostics {
            if diagnostic.severity != severity || self.is_suppressed(diagnostic) {
                continue;
            }

            if groups.iter().any(|group| group.matches(diagnostic)) {
                continue;
            }

            groups.push(ProblemGroup::new(diagnostic.clone()));
        }

        groups.len()
    }
}

/// One Problems presentation group after applying the current filters.
///
/// A group owns every source diagnostic so collapsing never loses detail and
/// expanded children preserve the existing click-navigation behavior.
struct ProblemGroup {
    /// Shared severity required for diagnostics to join this group.
    severity: Severity,
    /// Shared stable code shown in the collapsed summary.
    code: String,
    /// Shared navigation target; different targets remain separate groups.
    target: Option<DiagnosticTarget>,
    /// Original diagnostics retained in their source order.
    diagnostics: Vec<Diagnostic>,
}

impl ProblemGroup {
    /// Starts a group from its first source diagnostic.
    fn new(diagnostic: Diagnostic) -> Self {
        Self {
            severity: diagnostic.severity,
            code: diagnostic.code.clone(),
            target: diagnostic.target.clone(),
            diagnostics: vec![diagnostic],
        }
    }

    /// Returns whether a diagnostic belongs to this presentation group.
    fn matches(&self, diagnostic: &Diagnostic) -> bool {
        self.severity == diagnostic.severity
            && self.code == diagnostic.code
            && self.target == diagnostic.target
    }

    /// Formats the collapsed summary or the original single diagnostic row.
    fn summary_text(&self) -> String {
        if self.diagnostics.len() == 1 {
            problem_text(&self.diagnostics[0])
        } else {
            format!("[{}] {} occurrences", self.code, self.diagnostics.len())
        }
    }

    /// Builds clipboard text while retaining every original child message.
    fn copy_text(&self) -> String {
        if self.diagnostics.len() == 1 {
            return problem_text(&self.diagnostics[0]);
        }

        let mut lines = Vec::with_capacity(self.diagnostics.len() + 1);
        lines.push(self.summary_text());
        lines.extend(
            self.diagnostics
                .iter()
                .map(|diagnostic| format!("  - {}", diagnostic.message)),
        );
        lines.join("\n")
    }
}

/// Returns the shared visual color for a diagnostic severity.
fn severity_color(severity: Severity) -> egui::Color32 {
    match severity {
        Severity::Error => egui::Color32::from_rgb(220, 60, 60),
        Severity::Warning => egui::Color32::from_rgb(220, 180, 60),
        Severity::Info => egui::Color32::LIGHT_GRAY,
    }
}

/// Adds the code-level suppression command used by single rows and groups.
///
/// Errors intentionally receive no menu because blocking diagnostics must
/// always remain visible. The caller applies the requested mutation only after
/// the current render traversal completes.
fn add_suppression_menu(
    response: &egui::Response,
    severity: Severity,
    code: &str,
    suppress_code: &mut Option<String>,
) {
    if severity == Severity::Error {
        return;
    }

    response.context_menu(|ui| {
        if ui
            .button(format!("Suppress [{code}] from Problems"))
            .clicked()
        {
            *suppress_code = Some(code.to_owned());
            ui.close();
        }
    });
}

/// Formats a diagnostic using the Problems panel's stable visible layout.
///
/// Severity and timestamps are intentionally omitted because the existing
/// Problems rows display only the diagnostic code and message.
fn problem_text(diagnostic: &Diagnostic) -> String {
    format!("[{}] {}", diagnostic.code, diagnostic.message)
}

/// Compact label for one explicit Problems repair action.
fn repair_action_label(target: &DiagnosticTarget) -> &'static str {
    match target {
        DiagnosticTarget::Entity { .. } => "Select Entity",
        DiagnosticTarget::Component { .. } => "Reveal Component",
        DiagnosticTarget::Asset { .. } => "Open Asset",
        DiagnosticTarget::Graph { .. } => "Open Graph",
        DiagnosticTarget::Node { .. } => "Frame State",
        DiagnosticTarget::Edge { .. } => "Frame Edge",
        DiagnosticTarget::Port { .. } => "Frame Port",
        DiagnosticTarget::Group { .. } => "Frame Group",
        DiagnosticTarget::SourceFile { .. } => "Open Source",
    }
}

/// Describes a diagnostic navigation target for the row's hover tooltip.
fn target_hover_text(target: &DiagnosticTarget) -> String {
    match target {
        DiagnosticTarget::SourceFile { path, line } => line.map_or_else(
            || format!("Source file: {path}"),
            |line| format!("Source file: {path}:{line}"),
        ),
        DiagnosticTarget::Entity { id } => format!("Entity: {}", id.as_str()),
        DiagnosticTarget::Asset { id } => format!("Asset: {}", id.as_str()),
        DiagnosticTarget::Component {
            entity,
            component_type,
        } => format!(
            "Component {} on entity {}",
            component_type.as_str(),
            entity.as_str()
        ),
        DiagnosticTarget::Graph { id } => format!("Graph: {}", id.as_str()),
        DiagnosticTarget::Node { graph, node } => {
            format!("Node {} in graph {}", node.as_str(), graph.as_str())
        }
        DiagnosticTarget::Edge { graph, edge } => {
            format!("Edge {} in graph {}", edge.as_str(), graph.as_str())
        }
        DiagnosticTarget::Port { graph, node, port } => format!(
            "Port {} on node {} in graph {}",
            port.as_str(),
            node.as_str(),
            graph.as_str()
        ),
        DiagnosticTarget::Group { graph, group } => {
            format!("Group {} in graph {}", group.as_str(), graph.as_str())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that Copy All preserves diagnostic order and uses the same
    /// code-and-message representation as the Problems rows.
    #[test]
    fn visible_copy_text_contains_every_visible_problem_in_order() {
        let mut panel = ProblemsPanel::default();
        panel.set_problems(vec![
            Diagnostic::error("scene.missing_asset", "Required texture is missing"),
            Diagnostic::warning("scene.unused_entity", "Entity is not referenced"),
        ]);

        assert_eq!(
            panel.visible_copy_text().as_deref(),
            Some(
                "[scene.missing_asset] Required texture is missing\n\
                 [scene.unused_entity] Entity is not referenced"
            )
        );
    }

    /// Verifies that Copy All follows the same case-insensitive filter used by
    /// rendering and returns no payload when no visible row remains.
    #[test]
    fn visible_copy_text_respects_the_current_filter() {
        let mut panel = ProblemsPanel::default();
        panel.set_problems(vec![
            Diagnostic::error("scene.missing_asset", "Required texture is missing"),
            Diagnostic::warning("script.compile_warning", "Unused local variable"),
        ]);

        panel.filter = "COMPILE".to_owned();
        assert_eq!(
            panel.visible_copy_text().as_deref(),
            Some("[script.compile_warning] Unused local variable")
        );

        panel.filter = "does-not-exist".to_owned();
        assert_eq!(panel.visible_copy_text(), None);
    }

    /// Verifies independent severity switches and the quiet Info default.
    #[test]
    fn visible_groups_apply_independent_severity_filters() {
        let mut panel = ProblemsPanel::default();
        panel.set_problems(vec![
            Diagnostic::error("test.error", "error"),
            Diagnostic::warning("test.warning", "warning"),
            Diagnostic::info("test.info", "info"),
        ]);

        assert_eq!(panel.visible_groups().len(), 2);

        panel.severity_filter.errors = false;
        assert_eq!(panel.visible_groups().len(), 1);

        panel.severity_filter.warnings = false;
        panel.severity_filter.info = true;
        let groups = panel.visible_groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].severity, Severity::Info);
    }

    /// Verifies that repeated per-item messages compact into one presentation
    /// group when their severity, code, and target all match.
    #[test]
    fn same_code_and_target_form_one_group() {
        let mut panel = ProblemsPanel::default();
        panel.set_problems(vec![
            Diagnostic::warning("pmx.toon_shading_unsupported", "material 'body01'"),
            Diagnostic::warning("pmx.toon_shading_unsupported", "material 'body02'"),
            Diagnostic::warning("pmx.toon_shading_unsupported", "material 'body03'"),
        ]);

        let groups = panel.visible_groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].diagnostics.len(), 3);
        assert_eq!(
            groups[0].summary_text(),
            "[pmx.toon_shading_unsupported] 3 occurrences"
        );
    }

    /// Verifies that the same code on different source targets remains
    /// independently visible and navigable.
    #[test]
    fn same_code_on_different_targets_forms_separate_groups() {
        let mut first = Diagnostic::warning("asset.warning", "first asset");
        first.target = Some(DiagnosticTarget::SourceFile {
            path: "models/first.pmx".to_owned(),
            line: None,
        });
        let mut second = Diagnostic::warning("asset.warning", "second asset");
        second.target = Some(DiagnosticTarget::SourceFile {
            path: "models/second.pmx".to_owned(),
            line: None,
        });

        let mut panel = ProblemsPanel::default();
        panel.set_problems(vec![first, second]);

        assert_eq!(panel.visible_groups().len(), 2);
    }

    /// Verifies that a diagnostic mirrored into Console is counted once in
    /// the global status total.
    #[test]
    fn active_count_with_deduplicates_mirrored_diagnostic() {
        let diagnostic = Diagnostic::warning("asset.warning", "same warning");
        let mut panel = ProblemsPanel::default();
        panel.set_problems(vec![diagnostic.clone()]);

        assert_eq!(
            panel.active_count_with(Severity::Warning, &[diagnostic]),
            1
        );
    }

    /// Verifies that preferences can hide notices but never blocking errors.
    #[test]
    fn suppression_never_hides_errors() {
        let mut panel = ProblemsPanel::default();
        panel.set_suppressed_codes(["test.error".to_owned(), "test.warning".to_owned()]);
        panel.set_problems(vec![
            Diagnostic::error("test.error", "error"),
            Diagnostic::warning("test.warning", "warning"),
        ]);

        assert_eq!(panel.active_count(Severity::Error), 1);
        assert_eq!(panel.active_count(Severity::Warning), 0);
    }
}
