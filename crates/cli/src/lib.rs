//! Thin command-line adapter over the authoring API.
//!
//! The CLI owns argument parsing, project file selection, and JSON formatting
//! only. Domain behavior is delegated to shared authoring and asset services.

mod asset_cli;
mod capability_cli;
mod generic_cli;
mod prefab_cli;
mod scene_cli;
mod typed_document_cli;
mod vfx_cli;

use engine_authoring::{
    BehaviorTreeApply, BehaviorTreeAuthoringService, BehaviorTreeServiceError, Diagnostic, Graph,
    GraphChange, GraphCommand, PersistError, replace_file_contents,
};
use engine_mcp::ai_agent::{
    AiAgentInput, AiAgentOutput, ai_agent_tool_descriptors, handle_capture_frame,
    handle_inject_input, validate_ai_agent_input,
};
use serde::Serialize;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Runs the CLI adapter with command-line arguments excluding the executable name.
///
/// # Errors
///
/// Returns [`CliError`] when the command is unknown, serialization fails, or
/// the delegated authoring operation fails.
pub fn run_cli<I, S>(args: I) -> Result<String, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let result = run_cli_with_status(args)?;
    if result.exit_code == 0 {
        Ok(result.output)
    } else {
        Err(CliError::CommandFailed {
            output: result.output,
            exit_code: result.exit_code,
        })
    }
}

/// Runs the CLI adapter and returns both JSON/text output and process status.
///
/// # Errors
///
/// Returns [`CliError`] when the command is unknown, JSON formatting fails, or
/// file input cannot be read or deserialized.
pub fn run_cli_with_status<I, S>(args: I) -> Result<CliRunResult, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    if let Some(result) = capability_cli::dispatch(&args) {
        return result;
    }
    if let Some(result) = asset_cli::dispatch(&args) {
        return result;
    }
    if let Some(result) = generic_cli::dispatch(&args) {
        return result;
    }
    if let Some(result) = prefab_cli::dispatch(&args) {
        return result;
    }
    if let Some(result) = scene_cli::dispatch(&args) {
        return result;
    }
    if let Some(result) = typed_document_cli::dispatch(&args) {
        return result;
    }
    if let Some(result) = vfx_cli::dispatch(&args) {
        return result;
    }
    match args.as_slice() {
        [domain, command] if domain == "behavior-tree" && command == "schemas" => {
            Ok(CliRunResult::success(to_json(&behavior_tree_schemas())?))
        }
        [domain, command] if domain == "behavior-tree" && command == "example" => {
            Ok(CliRunResult::success(to_json(&behavior_tree_example()?)?))
        }
        [domain, command, path] if domain == "behavior-tree" && command == "validate" => {
            behavior_tree_validate(Path::new(path))
        }
        [domain, command, path] if domain == "behavior-tree" && command == "compile" => {
            behavior_tree_compile(Path::new(path))
        }
        [domain, command, path] if domain == "behavior-tree" && command == "layout" => {
            behavior_tree_layout(Path::new(path))
        }
        [domain, command, path] if domain == "behavior-tree" && command == "nodes" => {
            behavior_tree_nodes(Path::new(path))
        }
        [domain, command, path] if domain == "behavior-tree" && command == "edges" => {
            behavior_tree_edges(Path::new(path))
        }
        [domain, command, graph_path, commands_path]
            if domain == "behavior-tree" && command == "preview" =>
        {
            behavior_tree_preview(Path::new(graph_path), Path::new(commands_path))
        }
        [domain, command, graph_path, commands_path]
            if domain == "behavior-tree" && command == "apply" =>
        {
            behavior_tree_apply(Path::new(graph_path), Path::new(commands_path))
        }
        [domain, command] if domain == "ai-agent" && command == "describe-tools" => {
            ai_agent_describe_tools()
        }
        [domain, command, json_str] if domain == "ai-agent" && command == "validate-input" => {
            ai_agent_validate_input(json_str)
        }
        [domain, command, inbox_path, json_str]
            if domain == "ai-agent" && command == "inject-input" =>
        {
            ai_agent_inject_input(inbox_path, json_str)
        }
        [domain, command, inbox_path] if domain == "ai-agent" && command == "capture-frame" => {
            ai_agent_capture_frame(inbox_path)
        }
        [] => Ok(CliRunResult::success(help_text().to_owned())),
        [command] if command == "help" || command == "--help" || command == "-h" => {
            Ok(CliRunResult::success(help_text().to_owned()))
        }
        _ => Err(CliError::UnknownCommand {
            args: args.join(" "),
        }),
    }
}

/// Completed CLI command output and status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliRunResult {
    /// Text written to stdout.
    pub output: String,
    /// Process exit code.
    pub exit_code: i32,
}

impl CliRunResult {
    fn success(output: String) -> Self {
        Self {
            output,
            exit_code: 0,
        }
    }

    fn diagnostics(output: String, has_blocking_diagnostics: bool) -> Self {
        Self {
            output,
            exit_code: if has_blocking_diagnostics { 1 } else { 0 },
        }
    }

    fn input_error(output: String) -> Self {
        Self {
            output,
            exit_code: 2,
        }
    }
}

/// Reports why the CLI adapter could not complete a command.
#[derive(Debug)]
pub enum CliError {
    /// The requested command is not supported.
    UnknownCommand {
        /// Joined argument string supplied by the caller.
        args: String,
    },
    /// JSON serialization failed.
    Json(serde_json::Error),
    /// Authoring validation or compilation failed.
    Authoring {
        /// Structured diagnostics returned by the authoring layer.
        diagnostics: Vec<Diagnostic>,
    },
    /// Transaction commit failed because the source document changed.
    TransactionConflict {
        /// Human-readable conflict details from the authoring layer.
        message: String,
    },
    /// A status-bearing command returned non-success output.
    CommandFailed {
        /// Text written to stdout by the command.
        output: String,
        /// Process exit code.
        exit_code: i32,
    },
    /// File input could not be read.
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Source IO error.
        source: std::io::Error,
    },
    /// File persistence failed after command validation succeeded.
    Persist {
        /// Source persistence error.
        source: PersistError,
    },
    /// File input was not valid JSON for the requested document type.
    InvalidJson {
        /// Path that failed.
        path: PathBuf,
        /// Source JSON error.
        source: serde_json::Error,
    },
    /// File input is a valid graph document for another graph domain.
    WrongDomain {
        /// Path that failed.
        path: PathBuf,
        /// Expected graph kind.
        expected: String,
        /// Actual graph kind.
        actual: String,
    },
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand { args } => {
                write!(formatter, "unknown command `{args}`\n\n{}", help_text())
            }
            Self::Json(error) => write!(formatter, "failed to serialize CLI output: {error}"),
            Self::Authoring { diagnostics } => write!(
                formatter,
                "authoring operation failed with {} diagnostic(s)",
                diagnostics.len()
            ),
            Self::TransactionConflict { message } => {
                write!(formatter, "authoring transaction conflict: {message}")
            }
            Self::CommandFailed { output, .. } => formatter.write_str(output),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "{}",
                    input_error_json("io_error", path, &source.to_string())
                )
            }
            Self::Persist { source } => {
                write!(
                    formatter,
                    "{}",
                    input_error_json("io_error", source.path(), &source.message())
                )
            }
            Self::InvalidJson { path, source } => {
                write!(
                    formatter,
                    "{}",
                    input_error_json("invalid_json", path, &source.to_string())
                )
            }
            Self::WrongDomain {
                path,
                expected,
                actual,
            } => {
                let message = format!("expected graph kind `{expected}`, found `{actual}`");
                write!(
                    formatter,
                    "{}",
                    input_error_json("wrong_domain", path, &message)
                )
            }
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::Persist { source } => Some(source),
            Self::InvalidJson { source, .. } => Some(source),
            Self::UnknownCommand { .. }
            | Self::Authoring { .. }
            | Self::TransactionConflict { .. }
            | Self::CommandFailed { .. }
            | Self::WrongDomain { .. } => None,
        }
    }
}

impl CliError {
    /// Returns the process exit code for this error.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Io { .. }
            | Self::Persist { .. }
            | Self::InvalidJson { .. }
            | Self::WrongDomain { .. } => 2,
            Self::CommandFailed { exit_code, .. } => *exit_code,
            Self::UnknownCommand { .. }
            | Self::Json(_)
            | Self::Authoring { .. }
            | Self::TransactionConflict { .. } => 1,
        }
    }
}

#[derive(Debug, Serialize)]
struct QueryNodesOutput {
    success: bool,
    nodes: Vec<engine_authoring::BehaviorTreeNodeSummary>,
}

#[derive(Debug, Serialize)]
struct QueryEdgesOutput {
    success: bool,
    edges: Vec<engine_authoring::BehaviorTreeEdgeSummary>,
}

#[derive(Debug, Serialize)]
struct TransactionOutput<'a> {
    success: bool,
    diagnostics: &'a [Diagnostic],
    diff: &'a [GraphChange],
}

impl<'a> TransactionOutput<'a> {
    fn from_application(application: &'a BehaviorTreeApply) -> Self {
        Self {
            success: application.success,
            diagnostics: &application.diagnostics,
            diff: &application.diff,
        }
    }
}

#[derive(Debug, Serialize)]
struct InputErrorOutput<'a> {
    success: bool,
    error: InputErrorBody<'a>,
}

#[derive(Debug, Serialize)]
struct InputErrorBody<'a> {
    kind: &'a str,
    path: String,
    message: &'a str,
}

fn behavior_tree_schemas() -> engine_authoring::BehaviorTreeSchemaCatalog {
    BehaviorTreeAuthoringService::new().schemas()
}

fn behavior_tree_example() -> Result<engine_authoring::BehaviorTreeExample, CliError> {
    BehaviorTreeAuthoringService::new()
        .example()
        .map_err(|error| service_error_to_cli(error, None))
}

fn behavior_tree_validate(path: &Path) -> Result<CliRunResult, CliError> {
    let service = BehaviorTreeAuthoringService::new();
    let graph = match load_behavior_tree_graph(path, &service) {
        Ok(graph) => graph,
        Err(error) => return input_error_result(error),
    };
    let output = service.validate(&graph);
    let has_blocking_diagnostics = !output.success;

    Ok(CliRunResult::diagnostics(
        to_json(&output)?,
        has_blocking_diagnostics,
    ))
}

fn behavior_tree_compile(path: &Path) -> Result<CliRunResult, CliError> {
    let service = BehaviorTreeAuthoringService::new();
    let graph = match load_behavior_tree_graph(path, &service) {
        Ok(graph) => graph,
        Err(error) => return input_error_result(error),
    };
    let output = service.compile(&graph);
    let has_blocking_diagnostics = !output.success;

    Ok(CliRunResult::diagnostics(
        to_json(&output)?,
        has_blocking_diagnostics,
    ))
}

fn behavior_tree_layout(path: &Path) -> Result<CliRunResult, CliError> {
    let service = BehaviorTreeAuthoringService::new();
    let graph = match load_behavior_tree_graph(path, &service) {
        Ok(graph) => graph,
        Err(error) => return input_error_result(error),
    };
    let output = service.layout(&graph);
    let has_blocking_diagnostics = !output.success;

    Ok(CliRunResult::diagnostics(
        to_json(&output)?,
        has_blocking_diagnostics,
    ))
}

fn behavior_tree_nodes(path: &Path) -> Result<CliRunResult, CliError> {
    let service = BehaviorTreeAuthoringService::new();
    let graph = match load_behavior_tree_graph(path, &service) {
        Ok(graph) => graph,
        Err(error) => return input_error_result(error),
    };
    let output = QueryNodesOutput {
        success: true,
        nodes: service.nodes(&graph),
    };
    Ok(CliRunResult::success(to_json(&output)?))
}

fn behavior_tree_edges(path: &Path) -> Result<CliRunResult, CliError> {
    let service = BehaviorTreeAuthoringService::new();
    let graph = match load_behavior_tree_graph(path, &service) {
        Ok(graph) => graph,
        Err(error) => return input_error_result(error),
    };
    let output = QueryEdgesOutput {
        success: true,
        edges: service.edges(&graph),
    };
    Ok(CliRunResult::success(to_json(&output)?))
}

fn behavior_tree_preview(
    graph_path: &Path,
    commands_path: &Path,
) -> Result<CliRunResult, CliError> {
    behavior_tree_apply_impl(graph_path, commands_path, false)
}

fn behavior_tree_apply(graph_path: &Path, commands_path: &Path) -> Result<CliRunResult, CliError> {
    behavior_tree_apply_impl(graph_path, commands_path, true)
}

fn behavior_tree_apply_impl(
    graph_path: &Path,
    commands_path: &Path,
    persist: bool,
) -> Result<CliRunResult, CliError> {
    let service = BehaviorTreeAuthoringService::new();
    let graph = match load_behavior_tree_graph(graph_path, &service) {
        Ok(graph) => graph,
        Err(error) => return input_error_result(error),
    };
    let commands = match load_commands(commands_path) {
        Ok(cmds) => cmds,
        Err(error) => return input_error_result(error),
    };

    let application = service.apply(&graph, commands);
    if persist && let Some(graph) = application.graph() {
        let json = service
            .graph_to_canonical_json(graph)
            .map_err(|error| service_error_to_cli(error, Some(graph_path)))?;
        replace_file_contents(graph_path, &json).map_err(|source| CliError::Persist { source })?;
    }

    let output = TransactionOutput::from_application(&application);
    Ok(CliRunResult::diagnostics(
        to_json(&output)?,
        !application.success,
    ))
}

fn load_commands(path: &Path) -> Result<Vec<GraphCommand>, CliError> {
    let json = fs::read_to_string(path).map_err(|source| CliError::Io {
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_str(&json).map_err(|source| CliError::InvalidJson {
        path: path.to_owned(),
        source,
    })
}

fn load_graph(path: &Path) -> Result<Graph, CliError> {
    let json = fs::read_to_string(path).map_err(|source| CliError::Io {
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_str(&json).map_err(|source| CliError::InvalidJson {
        path: path.to_owned(),
        source,
    })
}

fn load_behavior_tree_graph(
    path: &Path,
    service: &BehaviorTreeAuthoringService,
) -> Result<Graph, CliError> {
    let graph = load_graph(path)?;
    let domain = service.domain();
    if graph.kind != *domain.graph_kind() {
        return Err(CliError::WrongDomain {
            path: path.to_owned(),
            expected: domain.graph_kind().as_str().to_owned(),
            actual: graph.kind.as_str().to_owned(),
        });
    }
    Ok(graph)
}

fn input_error_result(error: CliError) -> Result<CliRunResult, CliError> {
    match error {
        error @ (CliError::Io { .. }
        | CliError::InvalidJson { .. }
        | CliError::WrongDomain { .. }) => Ok(CliRunResult::input_error(error.to_string())),
        error => Err(error),
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<String, CliError> {
    serde_json::to_string_pretty(value).map_err(CliError::Json)
}

fn input_error_json(kind: &'static str, path: &Path, message: &str) -> String {
    let output = InputErrorOutput {
        success: false,
        error: InputErrorBody {
            kind,
            path: path.display().to_string(),
            message,
        },
    };
    serde_json::to_string_pretty(&output)
        .unwrap_or_else(|_| "{\"success\":false,\"error\":{\"kind\":\"internal_error\"}}".into())
}

fn transaction_error_to_cli(error: engine_authoring::GraphTransactionError) -> CliError {
    match error {
        engine_authoring::GraphTransactionError::ValidationFailed { diagnostics } => {
            CliError::Authoring { diagnostics }
        }
        engine_authoring::GraphTransactionError::Conflict { .. } => CliError::TransactionConflict {
            message: error.to_string(),
        },
    }
}

fn graph_save_error_to_cli(error: engine_authoring::GraphSaveError) -> CliError {
    match error {
        engine_authoring::GraphSaveError::ValidationFailed { diagnostics } => {
            CliError::Authoring { diagnostics }
        }
        engine_authoring::GraphSaveError::Json(error) => CliError::Json(error),
    }
}

fn service_error_to_cli(error: BehaviorTreeServiceError, path: Option<&Path>) -> CliError {
    match error {
        BehaviorTreeServiceError::Json { source } => CliError::Json(source),
        BehaviorTreeServiceError::WrongDomain { expected, actual } => CliError::WrongDomain {
            path: path
                .map(Path::to_owned)
                .unwrap_or_else(|| PathBuf::from("<in-memory>")),
            expected: expected.as_str().to_owned(),
            actual: actual.as_str().to_owned(),
        },
        BehaviorTreeServiceError::Diagnostics { diagnostics } => {
            CliError::Authoring { diagnostics }
        }
        BehaviorTreeServiceError::Transaction { source } => transaction_error_to_cli(source),
        BehaviorTreeServiceError::Save { source } => graph_save_error_to_cli(source),
    }
}

fn ai_agent_describe_tools() -> Result<CliRunResult, CliError> {
    let descriptors = ai_agent_tool_descriptors();
    Ok(CliRunResult::success(to_json(&descriptors)?))
}

fn ai_agent_validate_input(json_str: &str) -> Result<CliRunResult, CliError> {
    let input: AiAgentInput = serde_json::from_str(json_str).map_err(CliError::Json)?;
    let errors = validate_ai_agent_input(&input);
    let output = if errors.is_empty() {
        AiAgentOutput::ok("input is valid")
    } else {
        AiAgentOutput::err(errors.join("; "))
    };
    let has_blocking = !output.success;
    Ok(CliRunResult::diagnostics(to_json(&output)?, has_blocking))
}

fn ai_agent_inject_input(inbox_path: &str, json_str: &str) -> Result<CliRunResult, CliError> {
    let input: AiAgentInput = serde_json::from_str(json_str).map_err(CliError::Json)?;
    let errors = validate_ai_agent_input(&input);
    if !errors.is_empty() {
        let output = AiAgentOutput::err(errors.join("; "));
        return Ok(CliRunResult::diagnostics(to_json(&output)?, true));
    }
    let mcp_input = serde_json::json!({
        "inbox_path": inbox_path,
        "action": input.action,
        "payload": input.payload,
    });
    let result = serde_json::from_value::<AiAgentOutput>(handle_inject_input(mcp_input))
        .unwrap_or_else(|_| AiAgentOutput::err("internal error"));
    let has_blocking = !result.success;
    Ok(CliRunResult::diagnostics(to_json(&result)?, has_blocking))
}

fn ai_agent_capture_frame(inbox_path: &str) -> Result<CliRunResult, CliError> {
    let mcp_input = serde_json::json!({ "inbox_path": inbox_path });
    let result = serde_json::from_value::<AiAgentOutput>(handle_capture_frame(mcp_input))
        .unwrap_or_else(|_| AiAgentOutput::err("internal error"));
    let has_blocking = !result.success;
    Ok(CliRunResult::diagnostics(to_json(&result)?, has_blocking))
}

fn help_text() -> &'static str {
    "Usage:\n  engine-cli authoring list\n  engine-cli authoring capabilities\n  engine-cli authoring describe <capability-id>\n  engine-cli authoring inspect|validate|preview|apply <capability-id> [capability arguments]\n  engine-cli project describe <project-root>\n  engine-cli asset search <project-root> [query]\n  engine-cli asset inspect <project-root> <asset-id>\n  engine-cli prefab create <project-root> <scene-relative-path> <root-entity-id> <destination-relative-path>\n  engine-cli prefab preview <project-root> <scene-relative-path> <source-relative-path> [parent-entity-id]\n  engine-cli prefab instantiate <project-root> <scene-relative-path> <source-relative-path> [parent-entity-id]\n  engine-cli scene inspect <project-root> <scene-relative-path>\n  engine-cli scene validate <project-root> <scene-relative-path>\n  engine-cli scene preview <project-root> <scene-relative-path> <commands.json>\n  engine-cli scene apply <project-root> <scene-relative-path> <commands.json>\n  engine-cli entity find <project-root> <scene-relative-path> [query]\n  engine-cli entity inspect <project-root> <scene-relative-path> <entity-id>\n  engine-cli component schemas\n  engine-cli graph inspect <graph.json>\n  engine-cli graph validate <graph.json>\n  engine-cli graph preview <graph.json> <commands.json>\n  engine-cli graph apply <graph.json> <commands.json>\n  engine-cli graph layout inspect <graph.json> <graph.view.json>\n  engine-cli graph layout validate <graph.json> <graph.view.json>\n  engine-cli graph layout preview <graph.json> <graph.view.json> <commands.json>\n  engine-cli graph layout apply <graph.json> <graph.view.json> <commands.json>\n  engine-cli ui inspect <ui.json>\n  engine-cli ui validate <ui.json>\n  engine-cli ui preview <ui.json> <commands.json>\n  engine-cli ui apply <ui.json> <commands.json>\n  engine-cli material inspect <project-root> <asset-relative-path>\n  engine-cli material validate <project-root> <asset-relative-path>\n  engine-cli material preview <project-root> <asset-relative-path> <replacement.json>\n  engine-cli material apply <project-root> <asset-relative-path> <replacement.json>\n  engine-cli project_settings inspect <project-root>\n  engine-cli project_settings validate <project-root>\n  engine-cli project_settings preview <project-root> <replacement.json>\n  engine-cli project_settings apply <project-root> <replacement.json>\n  engine-cli animation_set inspect <project-root> <asset-relative-path>\n  engine-cli animation_set validate <project-root> <asset-relative-path>\n  engine-cli animation_set preview <project-root> <asset-relative-path> <replacement.json>\n  engine-cli animation_set apply <project-root> <asset-relative-path> <replacement.json>\n  engine-cli sprite_atlas inspect <project-root> <asset-relative-path>\n  engine-cli sprite_atlas validate <project-root> <asset-relative-path>\n  engine-cli sprite_atlas preview <project-root> <asset-relative-path> <replacement.json>\n  engine-cli sprite_atlas apply <project-root> <asset-relative-path> <replacement.json>\n  engine-cli sprite_animation inspect <project-root> <asset-relative-path>\n  engine-cli sprite_animation validate <project-root> <asset-relative-path>\n  engine-cli sprite_animation preview <project-root> <asset-relative-path> <replacement.json>\n  engine-cli sprite_animation apply <project-root> <asset-relative-path> <replacement.json>\n  engine-cli tile_set inspect <project-root> <asset-relative-path>\n  engine-cli tile_set validate <project-root> <asset-relative-path>\n  engine-cli tile_set preview <project-root> <asset-relative-path> <replacement.json>\n  engine-cli tile_set apply <project-root> <asset-relative-path> <replacement.json>\n  engine-cli tile_map inspect <project-root> <asset-relative-path>\n  engine-cli tile_map validate <project-root> <asset-relative-path>\n  engine-cli tile_map preview <project-root> <asset-relative-path> <replacement.json>\n  engine-cli tile_map apply <project-root> <asset-relative-path> <replacement.json>
  engine-cli timeline inspect <project-root> <asset-relative-path>
  engine-cli timeline validate <project-root> <asset-relative-path>
  engine-cli timeline preview <project-root> <asset-relative-path> <replacement.json>
  engine-cli timeline apply <project-root> <asset-relative-path> <replacement.json>\n  engine-cli vfx schemas\n  engine-cli vfx inspect <effect.vfx.json>\n  engine-cli vfx validate <effect.vfx.json>\n  engine-cli vfx preview <effect.vfx.json> <commands.json>\n  engine-cli vfx apply <effect.vfx.json> <commands.json>\n  engine-cli vfx create <template> <effect.vfx.json>\n  engine-cli behavior-tree schemas\n  engine-cli behavior-tree example\n  engine-cli behavior-tree validate <graph.json>\n  engine-cli behavior-tree compile <graph.json>\n  engine-cli behavior-tree layout <graph.json>\n  engine-cli behavior-tree nodes <graph.json>\n  engine-cli behavior-tree edges <graph.json>\n  engine-cli behavior-tree preview <graph.json> <commands.json>\n  engine-cli behavior-tree apply <graph.json> <commands.json>\n  engine-cli ai-agent describe-tools\n  engine-cli ai-agent validate-input <json>\n  engine-cli ai-agent inject-input <inbox_path> <json>\n  engine-cli ai-agent capture-frame <inbox_path>"
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{
        BehaviorTreeDomain, EdgeId, GraphDomain, GraphId, GraphKind, GraphTransaction, NodeId,
    };
    use serde_json::Value;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn schema_command_outputs_behavior_tree_node_schemas() {
        let output = run_cli(["behavior-tree", "schemas"]).expect("schema command must succeed");
        let json: Value = serde_json::from_str(&output).expect("output must be JSON");

        let expected = serde_json::to_value(BehaviorTreeAuthoringService::new().schemas())
            .expect("schema catalog must serialize");

        assert_eq!(json, expected);
    }

    #[test]
    fn example_command_runs_phase4_scenario_through_authoring_api() {
        let output = run_cli(["behavior-tree", "example"]).expect("example command must succeed");
        let json: Value = serde_json::from_str(&output).expect("output must be JSON");

        assert_eq!(json["diagnostics"].as_array().unwrap().len(), 0);
        assert_eq!(json["graph"]["nodes"].as_object().unwrap().len(), 6);
        assert_eq!(json["graph"]["edges"].as_object().unwrap().len(), 5);
        assert_eq!(json["preview_diff"].as_array().unwrap().len(), 11);
        assert_eq!(json["commit_diff"].as_array().unwrap().len(), 11);
        assert_eq!(json["compiled"]["root"]["kind"], "root");
        assert_eq!(json["view"]["layout_policy"], "behavior_tree.top_down");
        assert_eq!(json["view"]["nodes"].as_object().unwrap().len(), 6);
    }

    #[test]
    fn unknown_command_reports_error() {
        assert!(matches!(
            run_cli(["scene", "validate"]),
            Err(CliError::UnknownCommand { .. })
        ));
    }

    #[test]
    fn transaction_conflict_reports_conflict_details() {
        let domain = BehaviorTreeDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "conflict_test",
        );
        let stale_transaction = GraphTransaction::begin(&graph);
        let current_transaction = GraphTransaction::begin(&graph);
        current_transaction
            .commit(&mut graph, domain.schema_registry())
            .expect("empty current transaction should advance graph revision");

        let error = stale_transaction
            .commit(&mut graph, domain.schema_registry())
            .expect_err("stale transaction must conflict");
        let cli_error = transaction_error_to_cli(error);
        let message = cli_error.to_string();

        assert!(matches!(cli_error, CliError::TransactionConflict { .. }));
        assert!(message.contains("authoring transaction conflict"));
        assert!(message.contains("revision"));
        assert!(!message.contains("0 diagnostic"));
    }

    #[test]
    fn service_wrong_domain_error_preserves_cli_path() {
        let path = PathBuf::from("graphs/enemy.graph.json");
        let error = service_error_to_cli(
            BehaviorTreeServiceError::WrongDomain {
                expected: GraphKind::new("behavior_tree.graph"),
                actual: GraphKind::new("other.graph"),
            },
            Some(&path),
        );
        let json = parse_json(&error.to_string());

        assert_eq!(json["error"]["kind"], "wrong_domain");
        assert_eq!(json["error"]["path"], path.display().to_string());
    }

    #[test]
    fn empty_args_return_help_without_unknown_command_error() {
        let output = run_cli(std::iter::empty::<String>()).expect("empty args should show help");

        assert!(output.contains("engine-cli behavior-tree schemas"));
    }

    #[test]
    fn validate_valid_behavior_tree_file_reports_success() {
        let fixture = write_temp_graph("valid_validate", &valid_behavior_tree_graph());

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "validate".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("validate command must run");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 0);
        assert_eq!(json["success"], true);
        assert_eq!(json["diagnostics"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn validate_invalid_behavior_tree_file_reports_failure() {
        let fixture = write_temp_graph("invalid_validate", &invalid_behavior_tree_graph());

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "validate".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("validate command must return diagnostics JSON");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 1);
        assert_eq!(json["success"], false);
        assert!(
            json["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == "behavior_tree.missing_root")
        );
    }

    #[test]
    fn run_cli_returns_error_for_file_command_validation_failure() {
        let fixture = write_temp_graph("run_cli_invalid_validate", &invalid_behavior_tree_graph());

        let error = run_cli([
            "behavior-tree".to_owned(),
            "validate".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect_err("run_cli should preserve nonzero status as an error");
        let json = parse_json(&error.to_string());

        assert!(matches!(
            error,
            CliError::CommandFailed { exit_code: 1, .. }
        ));
        assert_eq!(error.exit_code(), 1);
        assert_eq!(json["success"], false);
    }

    #[test]
    fn compile_valid_behavior_tree_file_outputs_compiled_tree() {
        let fixture = write_temp_graph("valid_compile", &valid_behavior_tree_graph());

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "compile".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("compile command must run");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 0);
        assert_eq!(json["success"], true);
        assert_eq!(json["diagnostics"].as_array().unwrap().len(), 0);
        assert_eq!(json["compiled_tree"]["root"]["kind"], "root");
    }

    #[test]
    fn compile_invalid_behavior_tree_file_outputs_diagnostics() {
        let fixture = write_temp_graph("invalid_compile", &invalid_behavior_tree_graph());

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "compile".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("compile command must return diagnostics JSON");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 1);
        assert_eq!(json["success"], false);
        assert!(json["compiled_tree"].is_null());
        assert!(!json["diagnostics"].as_array().unwrap().is_empty());
    }

    #[test]
    fn layout_valid_behavior_tree_file_outputs_graph_view() {
        let fixture = write_temp_graph("valid_layout", &valid_behavior_tree_graph());

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "layout".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("layout command must run");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 0);
        assert_eq!(json["success"], true);
        assert_eq!(json["diagnostics"].as_array().unwrap().len(), 0);
        assert_eq!(
            json["graph_view"]["layout_policy"],
            "behavior_tree.top_down"
        );
        assert_eq!(json["graph_view"]["nodes"].as_object().unwrap().len(), 3);
    }

    #[test]
    fn layout_invalid_behavior_tree_file_outputs_diagnostics() {
        let fixture = write_temp_graph("invalid_layout", &invalid_behavior_tree_graph());

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "layout".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("layout command must return diagnostics JSON");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 1);
        assert_eq!(json["success"], false);
        assert!(json["graph_view"].is_null());
        assert!(!json["diagnostics"].as_array().unwrap().is_empty());
    }

    #[test]
    fn missing_file_reports_exit_code_two_error() {
        let missing = temp_path("missing_file");

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "validate".to_owned(),
            missing.display().to_string(),
        ])
        .expect("missing file should produce input error JSON");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 2);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["kind"], "io_error");
    }

    #[test]
    fn invalid_json_reports_exit_code_two_error() {
        let fixture = write_temp_text("invalid_json", "{ invalid json");

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "validate".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("invalid JSON should produce input error JSON");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 2);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["kind"], "invalid_json");
    }

    #[test]
    fn wrong_domain_graph_reports_exit_code_two_error() {
        let fixture = write_temp_graph("wrong_domain", &wrong_domain_graph());

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "validate".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("wrong-domain graph should produce input error JSON");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 2);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["kind"], "wrong_domain");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("behavior_tree.graph")
        );
    }

    #[test]
    fn file_based_output_is_deterministic_for_same_input() {
        let fixture = write_temp_graph("deterministic_validate", &valid_behavior_tree_graph());

        let first = run_cli_with_status([
            "behavior-tree".to_owned(),
            "validate".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("first validate should succeed");
        let second = run_cli_with_status([
            "behavior-tree".to_owned(),
            "validate".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("second validate should succeed");

        assert_eq!(first.output, second.output);
    }

    #[test]
    fn nodes_query_returns_all_nodes_with_stable_ids() {
        let fixture = write_temp_graph("nodes_query", &valid_behavior_tree_graph());

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "nodes".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("nodes command must run");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 0);
        assert_eq!(json["success"], true);
        assert_eq!(json["nodes"].as_array().unwrap().len(), 3);
        let first = &json["nodes"][0];
        assert!(first["id"].as_str().unwrap().starts_with("node_"));
        assert!(
            first["node_type"]
                .as_str()
                .unwrap()
                .starts_with("behavior_tree.")
        );
    }

    #[test]
    fn edges_query_returns_all_edges_with_endpoints() {
        let fixture = write_temp_graph("edges_query", &valid_behavior_tree_graph());

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "edges".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("edges command must run");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 0);
        assert_eq!(json["success"], true);
        assert_eq!(json["edges"].as_array().unwrap().len(), 2);
        let first = &json["edges"][0];
        assert!(first["id"].as_str().unwrap().starts_with("edge_"));
        assert!(first["from"]["node"].as_str().unwrap().starts_with("node_"));
        assert!(first["to"]["node"].as_str().unwrap().starts_with("node_"));
    }

    #[test]
    fn nodes_query_output_is_deterministic() {
        let fixture = write_temp_graph("nodes_deterministic", &valid_behavior_tree_graph());

        let first = run_cli_with_status([
            "behavior-tree".to_owned(),
            "nodes".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("first nodes call must succeed");
        let second = run_cli_with_status([
            "behavior-tree".to_owned(),
            "nodes".to_owned(),
            fixture.path().display().to_string(),
        ])
        .expect("second nodes call must succeed");

        assert_eq!(first.output, second.output);
    }

    #[test]
    fn preview_does_not_modify_input_file() {
        let domain = BehaviorTreeDomain::new();
        let fixture = write_temp_graph("preview_no_modify", &valid_behavior_tree_graph());
        let original = std::fs::read_to_string(fixture.path()).unwrap();

        let new_action = NodeId::generate();
        let commands = vec![GraphCommand::AddNode {
            node: domain.action_node(new_action, "extra_action"),
        }];
        let cmds_fixture = write_temp_text(
            "preview_no_modify_cmds",
            &serde_json::to_string_pretty(&commands).unwrap(),
        );

        run_cli_with_status([
            "behavior-tree".to_owned(),
            "preview".to_owned(),
            fixture.path().display().to_string(),
            cmds_fixture.path().display().to_string(),
        ])
        .expect("preview command must run");

        let after = std::fs::read_to_string(fixture.path()).unwrap();
        assert_eq!(original, after, "preview must not modify the graph file");
    }

    #[test]
    fn preview_returns_diff_and_diagnostics_for_valid_commands() {
        let domain = BehaviorTreeDomain::new();
        let graph = valid_behavior_tree_graph();
        let sequence_id = graph
            .nodes
            .iter()
            .find(|(_, n)| n.node_type == *domain.sequence_type())
            .map(|(id, _)| id.clone())
            .unwrap();
        let fixture = write_temp_graph("preview_valid", &graph);

        let new_action = NodeId::generate();
        let new_edge = EdgeId::generate();
        let commands = vec![
            GraphCommand::AddNode {
                node: domain.action_node(new_action.clone(), "extra_action"),
            },
            GraphCommand::AddEdge {
                edge: domain.child_edge(new_edge, sequence_id, new_action, 1),
            },
        ];
        let cmds_fixture = write_temp_text(
            "preview_valid_cmds",
            &serde_json::to_string_pretty(&commands).unwrap(),
        );

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "preview".to_owned(),
            fixture.path().display().to_string(),
            cmds_fixture.path().display().to_string(),
        ])
        .expect("preview command must run");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 0);
        assert_eq!(json["success"], true);
        assert_eq!(json["diagnostics"].as_array().unwrap().len(), 0);
        assert_eq!(json["diff"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn apply_writes_valid_transaction_to_file() {
        let domain = BehaviorTreeDomain::new();
        let graph = valid_behavior_tree_graph();
        let sequence_id = graph
            .nodes
            .iter()
            .find(|(_, n)| n.node_type == *domain.sequence_type())
            .map(|(id, _)| id.clone())
            .unwrap();
        let fixture = write_temp_graph("apply_writes", &graph);

        let new_action = NodeId::generate();
        let new_edge = EdgeId::generate();
        let commands = vec![
            GraphCommand::AddNode {
                node: domain.action_node(new_action.clone(), "extra_action"),
            },
            GraphCommand::AddEdge {
                edge: domain.child_edge(new_edge, sequence_id, new_action, 1),
            },
        ];
        let cmds_fixture = write_temp_text(
            "commit_writes_cmds",
            &serde_json::to_string_pretty(&commands).unwrap(),
        );

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "apply".to_owned(),
            fixture.path().display().to_string(),
            cmds_fixture.path().display().to_string(),
        ])
        .expect("commit command must run");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 0);
        assert_eq!(json["success"], true);
        assert_eq!(json["diff"].as_array().unwrap().len(), 2);

        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(fixture.path()).unwrap()).unwrap();
        assert_eq!(saved["nodes"].as_object().unwrap().len(), 4);
    }

    #[test]
    fn failed_structural_transaction_does_not_write() {
        let domain = BehaviorTreeDomain::new();
        let graph = valid_behavior_tree_graph();
        let existing_id = graph.nodes.keys().next().cloned().unwrap();
        let fixture = write_temp_graph("structural_fail_commit", &graph);
        let original = std::fs::read_to_string(fixture.path()).unwrap();

        let commands = vec![GraphCommand::AddNode {
            node: domain.action_node(existing_id, "duplicate"),
        }];
        let cmds_fixture = write_temp_text(
            "structural_fail_cmds",
            &serde_json::to_string_pretty(&commands).unwrap(),
        );

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "apply".to_owned(),
            fixture.path().display().to_string(),
            cmds_fixture.path().display().to_string(),
        ])
        .expect("commit command must return output");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 1);
        assert_eq!(json["success"], false);
        assert!(!json["diagnostics"].as_array().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(fixture.path()).unwrap(), original);
    }

    #[test]
    fn failed_domain_transaction_does_not_write() {
        let domain = BehaviorTreeDomain::new();
        let empty_graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "domain_fail_test",
        );
        let fixture = write_temp_graph("domain_fail_commit", &empty_graph);
        let original = std::fs::read_to_string(fixture.path()).unwrap();

        let commands = vec![GraphCommand::AddNode {
            node: domain.action_node(NodeId::generate(), "patrol"),
        }];
        let cmds_fixture = write_temp_text(
            "domain_fail_cmds",
            &serde_json::to_string_pretty(&commands).unwrap(),
        );

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "apply".to_owned(),
            fixture.path().display().to_string(),
            cmds_fixture.path().display().to_string(),
        ])
        .expect("commit command must return output");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 1);
        assert_eq!(json["success"], false);
        assert!(
            json["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == "behavior_tree.missing_root")
        );
        assert_eq!(std::fs::read_to_string(fixture.path()).unwrap(), original);
    }

    #[test]
    fn invalid_commands_json_returns_input_error() {
        let fixture = write_temp_graph("invalid_cmds_graph", &valid_behavior_tree_graph());
        let bad_cmds = write_temp_text("invalid_cmds_file", "{ not an array }");

        let result = run_cli_with_status([
            "behavior-tree".to_owned(),
            "preview".to_owned(),
            fixture.path().display().to_string(),
            bad_cmds.path().display().to_string(),
        ])
        .expect("preview must return input error JSON");
        let json = parse_json(&result.output);

        assert_eq!(result.exit_code, 2);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["kind"], "invalid_json");
    }

    #[test]
    fn preview_diff_output_is_deterministic() {
        let domain = BehaviorTreeDomain::new();
        let new_action = NodeId::generate();
        let commands = vec![GraphCommand::AddNode {
            node: domain.action_node(new_action, "patrol"),
        }];
        let cmds_json = serde_json::to_string_pretty(&commands).unwrap();

        let fixture = write_temp_graph("diff_deterministic", &valid_behavior_tree_graph());
        let cmds_fixture = write_temp_text("diff_deterministic_cmds", &cmds_json);

        let first = run_cli_with_status([
            "behavior-tree".to_owned(),
            "preview".to_owned(),
            fixture.path().display().to_string(),
            cmds_fixture.path().display().to_string(),
        ])
        .expect("first preview must succeed");
        let second = run_cli_with_status([
            "behavior-tree".to_owned(),
            "preview".to_owned(),
            fixture.path().display().to_string(),
            cmds_fixture.path().display().to_string(),
        ])
        .expect("second preview must succeed");

        assert_eq!(first.output, second.output);
    }

    fn valid_behavior_tree_graph() -> Graph {
        let domain = BehaviorTreeDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "valid_behavior_tree",
        );
        let root = NodeId::generate();
        let sequence = NodeId::generate();
        let action = NodeId::generate();
        graph
            .nodes
            .insert(root.clone(), domain.root_node(root.clone()));
        graph
            .nodes
            .insert(sequence.clone(), domain.sequence_node(sequence.clone()));
        graph
            .nodes
            .insert(action.clone(), domain.action_node(action.clone(), "idle"));
        let root_edge = domain.child_edge(EdgeId::generate(), root, sequence.clone(), 0);
        graph.edges.insert(root_edge.id.clone(), root_edge);
        let action_edge = domain.child_edge(EdgeId::generate(), sequence, action, 0);
        graph.edges.insert(action_edge.id.clone(), action_edge);
        graph
    }

    fn invalid_behavior_tree_graph() -> Graph {
        let domain = BehaviorTreeDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "invalid_behavior_tree",
        );
        let action = NodeId::generate();
        graph
            .nodes
            .insert(action.clone(), domain.action_node(action, "idle"));
        graph
    }

    fn wrong_domain_graph() -> Graph {
        Graph::new(
            GraphId::generate(),
            GraphKind::new("other_domain.graph"),
            "wrong_domain",
        )
    }

    struct TempGraphFile {
        path: PathBuf,
    }

    impl TempGraphFile {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempGraphFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn write_temp_graph(name: &str, graph: &Graph) -> TempGraphFile {
        let path = temp_path(name);
        let json = serde_json::to_string_pretty(graph).expect("test graph should serialize");
        std::fs::write(&path, json).expect("test fixture should write");
        TempGraphFile { path }
    }

    fn write_temp_text(name: &str, contents: &str) -> TempGraphFile {
        let path = temp_path(name);
        std::fs::write(&path, contents).expect("test fixture should write");
        TempGraphFile { path }
    }

    fn temp_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "engine_cli_{name}_{}_{}.json",
            std::process::id(),
            nonce
        ))
    }

    fn parse_json(output: &str) -> Value {
        serde_json::from_str(output).expect("output should be JSON")
    }
}
