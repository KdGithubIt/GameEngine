//! AI Studio controls for reproducible multi-model benchmark experiments.
//!
//! This module composes an experiment and reads its results; it never executes
//! one. Starting a suite launches the headless parent from
//! [`crate::benchmark_runner`] as a separate process, which then runs each
//! benchmark task in its own isolated Editor child.
//!
//! That separation is deliberate. An 84-run suite outlives an Editor session,
//! and a measured run must not share a process with a human-driven Editor. The
//! panel therefore polls the experiment directory on disk, so progress and the
//! final comparison survive closing and reopening AI Studio.

use super::*;
use crate::benchmark_comparison::{BenchmarkComparisonEquivalence, BenchmarkExperimentComparison};
use crate::benchmark_experiment::{
    BenchmarkExecutionOrder, BenchmarkExperimentSpec, ENGINE_COMMIT_HEAD,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// A suite that was started and is being watched on disk.
struct RunningSuite {
    process: Child,
    experiment_root: PathBuf,
    planned_runs: usize,
    finished: bool,
}

/// Everything the experiment section needs between frames.
pub(super) struct BenchmarkExperimentPanel {
    experiment_id: String,
    selected_models: BTreeSet<String>,
    custom_model: String,
    selected_tasks: BTreeSet<String>,
    repeat_count: u32,
    seeded_order: bool,
    seed: u64,
    stop_on_failure: bool,
    suite: Option<RunningSuite>,
    comparison: Option<BenchmarkExperimentComparison>,
    message: Option<String>,
}

impl Default for BenchmarkExperimentPanel {
    fn default() -> Self {
        Self {
            experiment_id: "local-model-comparison".to_owned(),
            selected_models: BTreeSet::new(),
            custom_model: String::new(),
            selected_tasks: BENCHMARK_TASKS
                .iter()
                .map(|task| task.id.to_owned())
                .collect(),
            repeat_count: 1,
            seeded_order: false,
            seed: 1,
            stop_on_failure: false,
            suite: None,
            comparison: None,
            message: None,
        }
    }
}

impl BenchmarkExperimentPanel {
    fn planned_runs(&self) -> usize {
        self.selected_models.len() * self.selected_tasks.len() * self.repeat_count as usize
    }
}

impl AiStudioPanel {
    /// Draws the reproducible model-comparison controls and their results.
    pub(super) fn show_benchmark_experiment(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Reproducible model comparison")
            .default_open(cfg!(feature = "visual-validation"))
            .show(ui, |ui| {
                ui.small(
                    "Runs the same fixture, tasks, policy, and completion criteria against each selected model, one isolated Editor child per run. Multi-model routing is disabled so every result stays attributable to its own model.",
                );
                self.show_experiment_identity(ui);
                ui.separator();
                self.show_experiment_models(ui);
                ui.separator();
                self.show_experiment_tasks(ui);
                ui.separator();
                self.show_experiment_controls(ui);
                self.show_experiment_progress(ui);
                self.show_experiment_comparison(ui);
            });
    }

    fn show_experiment_identity(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Experiment");
            ui.add(
                egui::TextEdit::singleline(&mut self.benchmark_experiment.experiment_id)
                    .desired_width(220.0),
            );
        });
        if ENGINE_COMMIT_HEAD.is_empty() {
            ui.small(
                "GameEngine commit: unavailable. This Editor was built without repository identity, so no comparable experiment can be started from it.",
            );
        } else {
            ui.small(format!(
                "GameEngine commit {ENGINE_COMMIT_HEAD} · corpus {BENCHMARK_CORPUS_VERSION} · endpoint {}",
                self.local_model_endpoint
            ));
        }
    }

    fn show_experiment_models(&mut self, ui: &mut egui::Ui) {
        ui.strong("Models");
        let discovered = self
            .current_installed_inventory()
            .map(|inventory| {
                inventory
                    .models
                    .iter()
                    .map(|model| {
                        let qualified = !self
                            .model_catalog
                            .profiles_for_model("ollama-compatible", &model.name)
                            .is_empty();
                        let representation = model
                            .quantization_level
                            .clone()
                            .unwrap_or_else(|| "quantization unavailable".to_owned());
                        (model.name.clone(), representation, qualified)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if discovered.is_empty() {
            ui.small(
                "No installed models discovered for this endpoint yet. Run discovery in Local model settings, or add an exact model name below.",
            );
        }
        for (name, representation, qualified) in discovered {
            let mut selected = self.benchmark_experiment.selected_models.contains(&name);
            let label = format!(
                "{name} · {representation} · {}",
                if qualified {
                    "benchmark-qualified"
                } else {
                    "compatible / unverified"
                }
            );
            if ui.checkbox(&mut selected, label).changed() {
                if selected {
                    self.benchmark_experiment.selected_models.insert(name);
                } else {
                    self.benchmark_experiment.selected_models.remove(&name);
                }
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Exact model");
            ui.add(
                egui::TextEdit::singleline(&mut self.benchmark_experiment.custom_model)
                    .hint_text("family:tag")
                    .desired_width(200.0),
            );
            let addable = !self.benchmark_experiment.custom_model.trim().is_empty();
            if ui.add_enabled(addable, egui::Button::new("Add")).clicked() {
                let model = self.benchmark_experiment.custom_model.trim().to_owned();
                self.benchmark_experiment.selected_models.insert(model);
                self.benchmark_experiment.custom_model.clear();
            }
        });
        let selected = self
            .benchmark_experiment
            .selected_models
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        if selected.is_empty() {
            ui.small("No model selected.");
        } else {
            for model in selected {
                ui.horizontal(|ui| {
                    ui.small(&model);
                    if ui.small_button("Remove").clicked() {
                        self.benchmark_experiment.selected_models.remove(&model);
                    }
                });
            }
        }
        ui.small(
            "A quantization difference is a different representation and is compared as its own entry, never merged into one model name.",
        );
    }

    fn show_experiment_tasks(&mut self, ui: &mut egui::Ui) {
        ui.strong("Tasks");
        for task in BENCHMARK_TASKS {
            let mut selected = self.benchmark_experiment.selected_tasks.contains(task.id);
            if ui.checkbox(&mut selected, task.label).changed() {
                if selected {
                    self.benchmark_experiment
                        .selected_tasks
                        .insert(task.id.to_owned());
                } else {
                    self.benchmark_experiment.selected_tasks.remove(task.id);
                }
            }
        }
    }

    fn show_experiment_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Repetitions");
            ui.add(egui::DragValue::new(&mut self.benchmark_experiment.repeat_count).range(1..=20));
            ui.checkbox(
                &mut self.benchmark_experiment.stop_on_failure,
                "Stop on first failure",
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(
                &mut self.benchmark_experiment.seeded_order,
                "Randomize run order",
            );
            if self.benchmark_experiment.seeded_order {
                ui.label("Seed");
                ui.add(egui::DragValue::new(&mut self.benchmark_experiment.seed));
                ui.small("The seed is recorded, so the order stays reproducible.");
            } else {
                ui.small("Deterministic order: model, then task, then repetition.");
            }
        });
        let planned = self.benchmark_experiment.planned_runs();
        ui.small(format!(
            "{} model(s) x {} task(s) x {} repetition(s) = {planned} run(s)",
            self.benchmark_experiment.selected_models.len(),
            self.benchmark_experiment.selected_tasks.len(),
            self.benchmark_experiment.repeat_count,
        ));
        let running = self.benchmark_experiment.suite.is_some();
        let startable = !running
            && planned > 0
            && !ENGINE_COMMIT_HEAD.is_empty()
            && !self.benchmark_experiment.experiment_id.trim().is_empty();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(startable, egui::Button::new("Start experiment"))
                .clicked()
            {
                self.start_benchmark_experiment();
            }
            if ui.add_enabled(running, egui::Button::new("Stop")).clicked() {
                self.stop_benchmark_experiment();
            }
        });
        if let Some(message) = self.benchmark_experiment.message.clone() {
            ui.small(message);
        }
    }

    fn show_experiment_progress(&mut self, ui: &mut egui::Ui) {
        let Some(suite) = self.benchmark_experiment.suite.as_ref() else {
            return;
        };
        let recorded = count_recorded_runs(&suite.experiment_root);
        ui.separator();
        ui.strong(if suite.finished {
            "Experiment finished"
        } else {
            "Experiment running"
        });
        ui.small(format!(
            "{recorded} of {} planned run(s) recorded",
            suite.planned_runs
        ));
        if suite.planned_runs > 0 {
            ui.add(
                egui::ProgressBar::new(recorded as f32 / suite.planned_runs as f32)
                    .show_percentage(),
            );
        }
        ui.small(format!("Results: {}", suite.experiment_root.display()));
    }

    fn show_experiment_comparison(&mut self, ui: &mut egui::Ui) {
        let Some(comparison) = self.benchmark_experiment.comparison.clone() else {
            return;
        };
        ui.separator();
        ui.strong("Comparison");
        match &comparison.equivalence {
            BenchmarkComparisonEquivalence::EquivalentModelComparison => {
                ui.small("Model-only comparison: every other dimension matched.");
            }
            BenchmarkComparisonEquivalence::NonEquivalent { differences } => {
                ui.small(format!(
                    "Non-equivalent comparison; these dimensions also changed: {}. This is not a model-only ranking.",
                    differences.join(", ")
                ));
            }
            BenchmarkComparisonEquivalence::InsufficientEvidence { reason } => {
                ui.small(format!("Comparison unavailable: {reason}"));
            }
        }
        egui::ScrollArea::horizontal()
            .id_salt("ai_studio_benchmark_comparison")
            .show(ui, |ui| {
                egui::Grid::new("ai_studio_benchmark_comparison_grid")
                    .striped(true)
                    .show(ui, |ui| {
                        for heading in [
                            "Model",
                            "Passed",
                            "Completion gate",
                            "Turns",
                            "Tool calls",
                            "Invalid",
                            "Repairs",
                            "Validations",
                            "Elapsed ms",
                            "Load ms",
                            "TTFT ms",
                            "Tokens/s",
                            "OOM",
                            "Backend",
                            "Runtime",
                            "Visual",
                        ] {
                            ui.strong(heading);
                        }
                        ui.end_row();
                        for model in &comparison.models {
                            ui.label(&model.model_id);
                            ui.label(format!("{}/{}", model.passed_runs, model.planned_runs));
                            ui.label(rate_label(model.completion_gate_success.permille()));
                            ui.label(mean_label(model.model_turns.measured_mean_milli()));
                            ui.label(mean_label(model.tool_calls.measured_mean_milli()));
                            ui.label(mean_label(
                                model.invalid_or_failed_tool_calls.measured_mean_milli(),
                            ));
                            ui.label(mean_label(model.repair_loops.measured_mean_milli()));
                            ui.label(mean_label(model.validation_attempts.measured_mean_milli()));
                            ui.label(mean_label(model.elapsed_ms.measured_mean_milli()));
                            ui.label(mean_label(model.load_latency_ms.measured_mean_milli()));
                            ui.label(mean_label(model.ttft_ms.measured_mean_milli()));
                            ui.label(mean_label(
                                model
                                    .generation_tokens_per_second_milli
                                    .measured_mean_milli()
                                    .map(|value| value / 1000),
                            ));
                            ui.label(model.out_of_memory_failures.to_string());
                            ui.label(model.backend_failures.to_string());
                            ui.label(rate_label(model.runtime_interaction_success.permille()));
                            ui.label(rate_label(model.visual_evaluation_success.permille()));
                            ui.end_row();
                        }
                    });
            });
        ui.small(
            "An unavailable cell means no run measured that value. It is never shown as zero, and a value no run measured cannot support a catalog recommendation.",
        );
        if comparison.supports_recommendation() {
            ui.small(
                "Evidence is complete and comparable; these results may qualify a curated catalog recommendation.",
            );
        } else {
            ui.small(
                "Evidence is incomplete or non-equivalent, so no Lightweight, Balanced, or High Quality recommendation is derived from it.",
            );
        }
    }

    /// Freezes the configured experiment and starts its headless parent.
    fn start_benchmark_experiment(&mut self) {
        match self.spawn_benchmark_experiment() {
            Ok(suite) => {
                self.benchmark_experiment.comparison = None;
                self.benchmark_experiment.message = Some(format!(
                    "Started {} run(s); each run gets a fresh fixture and its own Editor child.",
                    suite.planned_runs
                ));
                self.benchmark_experiment.suite = Some(suite);
            }
            Err(error) => {
                self.benchmark_experiment.message = Some(format!("Experiment not started: {error}"))
            }
        }
    }

    fn spawn_benchmark_experiment(&mut self) -> Result<RunningSuite, String> {
        if ENGINE_COMMIT_HEAD.is_empty() {
            return Err("this Editor build carries no exact GameEngine commit identity".to_owned());
        }
        let panel = &self.benchmark_experiment;
        let mut spec = BenchmarkExperimentSpec::local_single_model_comparison(
            panel.experiment_id.trim(),
            ENGINE_COMMIT_HEAD,
            panel.selected_models.iter().cloned().collect(),
            BENCHMARK_TASKS
                .iter()
                .filter(|task| panel.selected_tasks.contains(task.id))
                .map(|task| task.id.to_owned())
                .collect(),
            panel.repeat_count,
            self.quality_preference,
            self.benchmark_experiment_root.clone(),
        );
        spec.stop_on_failure = panel.stop_on_failure;
        if panel.seeded_order {
            spec.execution_order = BenchmarkExecutionOrder::SeededInterleaved { seed: panel.seed };
        }
        let planned_runs = spec.planned_runs()?.len();
        let experiment_root = self
            .benchmark_experiment_root
            .join(experiment_directory_name(&spec.experiment_id));
        fs::create_dir_all(&experiment_root).map_err(|error| error.to_string())?;
        let spec_path = experiment_root.join("requested-experiment.json");
        let bytes = serde_json::to_vec_pretty(&spec).map_err(|error| error.to_string())?;
        fs::write(&spec_path, bytes).map_err(|error| error.to_string())?;

        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let process = Command::new(executable)
            .arg("--benchmark-experiment")
            .arg(&spec_path)
            .arg("--benchmark-endpoint")
            .arg(self.local_model_endpoint.trim())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start the benchmark experiment parent: {error}"))?;
        Ok(RunningSuite {
            process,
            experiment_root,
            planned_runs,
            finished: false,
        })
    }

    fn stop_benchmark_experiment(&mut self) {
        let Some(suite) = self.benchmark_experiment.suite.as_mut() else {
            return;
        };
        match suite.process.kill() {
            Ok(()) => {
                let _ = suite.process.wait();
                suite.finished = true;
                self.benchmark_experiment.message = Some(
                    "Experiment stopped. Runs already recorded remain, and the comparison reports the missing runs."
                        .to_owned(),
                );
            }
            Err(error) => {
                self.benchmark_experiment.message = Some(format!("Could not stop: {error}"))
            }
        }
    }

    /// Notices that a started experiment has finished and loads its comparison.
    pub(super) fn poll_benchmark_experiment(&mut self) {
        let Some(suite) = self.benchmark_experiment.suite.as_mut() else {
            return;
        };
        if suite.finished {
            return;
        }
        match suite.process.try_wait() {
            Ok(Some(_)) => suite.finished = true,
            Ok(None) => return,
            Err(error) => {
                self.benchmark_experiment.message =
                    Some(format!("Experiment status unavailable: {error}"));
                suite.finished = true;
            }
        }
        let comparison_path = suite.experiment_root.join("comparison.json");
        match read_comparison(&comparison_path) {
            Ok(comparison) => {
                self.benchmark_experiment.message = Some(format!(
                    "Experiment finished; comparison written to {}.",
                    comparison_path.display()
                ));
                self.benchmark_experiment.comparison = Some(comparison);
            }
            Err(error) => {
                self.benchmark_experiment.message =
                    Some(format!("Experiment produced no comparison: {error}"))
            }
        }
    }
}

fn read_comparison(path: &Path) -> Result<BenchmarkExperimentComparison, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn count_recorded_runs(experiment_root: &Path) -> usize {
    let Ok(entries) = fs::read_dir(experiment_root.join("runs")) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == std::ffi::OsStr::new("json"))
        })
        .count()
}

/// Mirrors the directory naming the experiment store uses on disk.
pub(super) fn experiment_directory_name(experiment_id: &str) -> String {
    let name = experiment_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if name.is_empty() {
        "experiment".to_owned()
    } else {
        name
    }
}

fn rate_label(permille: Option<u64>) -> String {
    match permille {
        Some(value) => format!("{}.{}%", value / 10, value % 10),
        None => "unavailable".to_owned(),
    }
}

fn mean_label(mean_milli: Option<u64>) -> String {
    match mean_milli {
        Some(value) => format!("{}.{:03}", value / 1000, value % 1000),
        None => "unavailable".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ui_experiment_directory_matches_the_store_layout() {
        assert_eq!(experiment_directory_name("local model/1"), "local-model-1");
        assert_eq!(experiment_directory_name(""), "experiment");
        assert_eq!(experiment_directory_name("keep_me-1"), "keep_me-1");
    }

    #[test]
    fn an_unmeasured_cell_is_labeled_unavailable_rather_than_zero() {
        assert_eq!(mean_label(None), "unavailable");
        assert_eq!(rate_label(None), "unavailable");
        assert_eq!(mean_label(Some(0)), "0.000");
        assert_eq!(rate_label(Some(0)), "0.0%");
    }

    #[test]
    fn every_task_is_selected_by_default_so_a_suite_covers_the_whole_corpus() {
        let panel = BenchmarkExperimentPanel::default();
        assert_eq!(panel.selected_tasks.len(), BENCHMARK_TASKS.len());
        assert_eq!(panel.planned_runs(), 0);
    }

    #[test]
    fn planned_runs_multiply_models_tasks_and_repetitions() {
        let panel = BenchmarkExperimentPanel {
            selected_models: ["a", "b", "c", "d"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            repeat_count: 3,
            ..BenchmarkExperimentPanel::default()
        };
        assert_eq!(panel.planned_runs(), 84);
    }
}
