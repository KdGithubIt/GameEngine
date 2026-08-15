//! Console panel for structured diagnostic output (Phase 30).

use eframe::egui;
use engine_authoring::{Diagnostic, DiagnosticTarget, Severity};

/// Severity filter flags for the Console panel.
#[derive(Debug)]
pub struct SeverityFilter {
    /// Show error-severity diagnostics.
    pub errors: bool,
    /// Show warning-severity diagnostics.
    pub warnings: bool,
    /// Show info-severity diagnostics.
    pub info: bool,
}

impl Default for SeverityFilter {
    fn default() -> Self {
        Self {
            errors: true,
            warnings: true,
            info: true,
        }
    }
}

impl SeverityFilter {
    fn accepts(&self, severity: Severity) -> bool {
        match severity {
            Severity::Error => self.errors,
            Severity::Warning => self.warnings,
            Severity::Info => self.info,
        }
    }
}

/// Result of drawing the Console panel for one frame.
#[derive(Default)]
pub struct ConsoleOutput {
    /// Target of a clicked diagnostic, for editor navigation.
    pub navigate_to: Option<DiagnosticTarget>,
    /// The user pressed Clear; the owner should drop its diagnostics.
    pub clear_requested: bool,
}

/// Panel that displays structured diagnostics with per-severity filtering,
/// text search, duplicate collapsing, and copy support.
#[derive(Default)]
pub struct ConsolePanel {
    filter: SeverityFilter,
    search: String,
    collapse: bool,
}

impl ConsolePanel {
    /// Draws the Console panel contents into `ui`, showing diagnostics from `diagnostics`.
    ///
    /// Returns the clicked entry's [`DiagnosticTarget`] (so the caller can
    /// navigate the editor) and whether the user requested a clear.
    pub fn show(&mut self, ui: &mut egui::Ui, diagnostics: &[Diagnostic]) -> ConsoleOutput {
        let mut output = ConsoleOutput::default();
        ui.horizontal(|ui| {
            ui.heading("Console");
            ui.separator();
            let e_label = egui::RichText::new("E").color(severity_color(Severity::Error));
            let w_label = egui::RichText::new("W").color(severity_color(Severity::Warning));
            let i_label = egui::RichText::new("I").color(severity_color(Severity::Info));
            ui.toggle_value(&mut self.filter.errors, e_label)
                .on_hover_text("Errors");
            ui.toggle_value(&mut self.filter.warnings, w_label)
                .on_hover_text("Warnings");
            ui.toggle_value(&mut self.filter.info, i_label)
                .on_hover_text("Info");
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Filter...")
                    .desired_width(160.0),
            );
            ui.toggle_value(&mut self.collapse, "Collapse")
                .on_hover_text("Group identical messages and show a count");
            if ui.button("Clear").clicked() {
                output.clear_requested = true;
            }
            let rows = self.visible_rows(diagnostics);
            if ui
                .add_enabled(!rows.is_empty(), egui::Button::new("Copy All"))
                .on_hover_text("Copy every visible line to the clipboard")
                .clicked()
            {
                let text = rows
                    .iter()
                    .map(|row| row.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                ui.ctx().copy_text(text);
            }
        });

        let rows = self.visible_rows(diagnostics);
        egui::ScrollArea::vertical()
            .id_salt("console_scroll")
            // The Console is the active contents of a resizable utility dock.
            // Keep its body as wide and tall as that dock instead of shrinking
            // to one message or stopping at the previous 160-point cap.
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if rows.is_empty() {
                    ui.label("No diagnostics");
                    return;
                }

                for row in &rows {
                    let text = egui::RichText::new(&row.text).color(severity_color(row.severity));
                    // Wrap long paths and messages at the dock width while
                    // retaining the conventional left-aligned log layout.
                    let response = ui.add(
                        egui::Label::new(text)
                            .wrap_mode(egui::TextWrapMode::Wrap)
                            .sense(egui::Sense::click()),
                    );
                    if row.target.is_some() && response.clicked() {
                        output.navigate_to = row.target.clone();
                    }
                    response.context_menu(|ui| {
                        if ui.button("Copy").clicked() {
                            ui.ctx().copy_text(row.text.clone());
                            ui.close();
                        }
                    });
                }
            });

        output
    }

    /// Applies severity, text, and collapse filters, producing display rows.
    fn visible_rows(&self, diagnostics: &[Diagnostic]) -> Vec<ConsoleRow> {
        let needle = self.search.trim().to_ascii_lowercase();
        let mut rows: Vec<ConsoleRow> = Vec::new();
        for diagnostic in diagnostics {
            if !self.filter.accepts(diagnostic.severity) {
                continue;
            }
            if !needle.is_empty()
                && !diagnostic.code.to_ascii_lowercase().contains(&needle)
                && !diagnostic.message.to_ascii_lowercase().contains(&needle)
            {
                continue;
            }
            let ts = diagnostic
                .timestamp_ms
                .map(|ms| format!("[{:.1}s] ", ms as f64 / 1000.0))
                .unwrap_or_default();
            let base = format!(
                "{ts}{} [{}] {}",
                severity_prefix(diagnostic.severity),
                diagnostic.code,
                diagnostic.message
            );
            if self.collapse {
                // Collapsing keys on code+message only so repeated events at
                // different timestamps still merge into one counted row.
                if let Some(existing) = rows.iter_mut().find(|row| {
                    row.collapse_key
                        .as_ref()
                        .is_some_and(|key| key.0 == diagnostic.code && key.1 == diagnostic.message)
                }) {
                    existing.count += 1;
                    existing.text = format!("{} (x{})", existing.base_text, existing.count);
                    continue;
                }
                rows.push(ConsoleRow {
                    base_text: base.clone(),
                    text: base,
                    severity: diagnostic.severity,
                    target: diagnostic.target.clone(),
                    collapse_key: Some((diagnostic.code.clone(), diagnostic.message.clone())),
                    count: 1,
                });
            } else {
                rows.push(ConsoleRow {
                    base_text: base.clone(),
                    text: base,
                    severity: diagnostic.severity,
                    target: diagnostic.target.clone(),
                    collapse_key: None,
                    count: 1,
                });
            }
        }
        rows
    }
}

struct ConsoleRow {
    base_text: String,
    text: String,
    severity: Severity,
    target: Option<DiagnosticTarget>,
    collapse_key: Option<(String, String)>,
    count: u32,
}

fn severity_prefix(s: Severity) -> &'static str {
    match s {
        Severity::Error => "ERR",
        Severity::Warning => "WRN",
        Severity::Info => "INF",
    }
}

fn severity_color(s: Severity) -> egui::Color32 {
    match s {
        Severity::Error => egui::Color32::from_rgb(220, 60, 60),
        Severity::Warning => egui::Color32::from_rgb(220, 180, 60),
        Severity::Info => egui::Color32::from_rgb(150, 200, 255),
    }
}
