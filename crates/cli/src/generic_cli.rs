//! Thin file-oriented CLI adapter for generic Graph, GraphView, and UI authoring.

use super::{CliError, CliRunResult, to_json};
use engine_authoring::{
    AuthoringGraphDomain, AuthoringPermission, AuthoringPermissions, Diagnostic, Graph,
    GraphAuthoringService, GraphCommand, GraphDomain, GraphView, GraphViewAuthoringService,
    GraphViewCommand, TimelineAuthoringCommand, TimelineAuthoringService, UiAuthoringService,
    UiAuthoringSession, UiDocument, UiDocumentCommand, load_timeline, replace_file_contents,
};
use serde::de::DeserializeOwned;
use std::fs;
use std::path::Path;

pub(super) fn dispatch(args: &[String]) -> Option<Result<CliRunResult, CliError>> {
    match args {
        [domain, command, path] if domain == "graph" && command == "inspect" => {
            Some(graph_inspect(Path::new(path)))
        }
        [domain, command, path] if domain == "graph" && command == "validate" => {
            Some(graph_validate(Path::new(path)))
        }
        [domain, command, graph_path, commands_path]
            if domain == "graph" && command == "preview" =>
        {
            Some(graph_mutate(
                Path::new(graph_path),
                Path::new(commands_path),
                false,
            ))
        }
        [domain, command, graph_path, commands_path]
            if domain == "graph" && command == "apply" =>
        {
            Some(graph_mutate(
                Path::new(graph_path),
                Path::new(commands_path),
                true,
            ))
        }
        [domain, area, command, graph_path, view_path]
            if domain == "graph" && area == "layout" && command == "inspect" =>
        {
            Some(graph_view_inspect(
                Path::new(graph_path),
                Path::new(view_path),
            ))
        }
        [domain, area, command, graph_path, view_path]
            if domain == "graph" && area == "layout" && command == "validate" =>
        {
            Some(graph_view_validate(
                Path::new(graph_path),
                Path::new(view_path),
            ))
        }
        [domain, area, command, graph_path, view_path, commands_path]
            if domain == "graph" && area == "layout" && command == "preview" =>
        {
            Some(graph_view_mutate(
                Path::new(graph_path),
                Path::new(view_path),
                Path::new(commands_path),
                false,
            ))
        }
        [domain, area, command, graph_path, view_path, commands_path]
            if domain == "graph" && area == "layout" && command == "apply" =>
        {
            Some(graph_view_mutate(
                Path::new(graph_path),
                Path::new(view_path),
                Path::new(commands_path),
                true,
            ))
        }
        [domain, command, path] if domain == "ui" && command == "inspect" => {
            Some(ui_inspect(Path::new(path)))
        }
        [domain, command, path] if domain == "ui" && command == "validate" => {
            Some(ui_validate(Path::new(path)))
        }
        [domain, command, path, commands_path] if domain == "ui" && command == "preview" => {
            Some(ui_mutate(
                Path::new(path),
                Path::new(commands_path),
                false,
            ))
        }
        [domain, command, path, commands_path] if domain == "ui" && command == "apply" => {
            Some(ui_mutate(
                Path::new(path),
                Path::new(commands_path),
                true,
            ))
        }
        [domain, command, path] if domain == "timeline" && command == "inspect" => {
            Some(timeline_inspect(Path::new(path)))
        }
        [domain, command, path] if domain == "timeline" && command == "validate" => {
            Some(timeline_validate(Path::new(path)))
        }
        [domain, command, path, commands_path]
            if domain == "timeline" && (command == "preview" || command == "apply") =>
        {
            Some(timeline_mutate(
                Path::new(path),
                Path::new(commands_path),
                command == "apply",
            ))
        }
        _ if args.first().is_some_and(|value| {
            value == "graph" || value == "ui" || value == "timeline"
        }) => {
            Some(Err(CliError::UnknownCommand {
                args: args.join(" "),
            }))
        }
        _ => None,
    }
}

fn graph_inspect(path: &Path) -> Result<CliRunResult, CliError> {
    let graph: Graph = load_json(path)?;
    let output = GraphAuthoringService::new()
        .inspect(&graph, &read_permissions())
        .map_err(authoring_error)?;
    Ok(CliRunResult::success(to_json(&output)?))
}

fn graph_validate(path: &Path) -> Result<CliRunResult, CliError> {
    let graph: Graph = load_json(path)?;
    let domain = AuthoringGraphDomain::for_graph(&graph).map_err(authoring_error)?;
    let output = GraphAuthoringService::new()
        .validate(&graph, &domain, &read_permissions())
        .map_err(authoring_error)?;
    Ok(CliRunResult::diagnostics(
        to_json(&output)?,
        !output.success,
    ))
}

fn graph_mutate(
    graph_path: &Path,
    commands_path: &Path,
    persist: bool,
) -> Result<CliRunResult, CliError> {
    let mut graph: Graph = load_json(graph_path)?;
    let commands: Vec<GraphCommand> = load_json(commands_path)?;
    let domain = AuthoringGraphDomain::for_graph(&graph).map_err(authoring_error)?;
    let service = GraphAuthoringService::new();
    let permissions = writable_permissions();
    let base = service
        .inspect(&graph, &permissions)
        .map_err(authoring_error)?;
    let output = if persist {
        service.apply(
            &mut graph,
            &domain,
            &permissions,
            base.revision,
            base.generation,
            commands,
        )
    } else {
        service.preview(
            &graph,
            &domain,
            &permissions,
            base.revision,
            base.generation,
            commands,
        )
    }
    .map_err(authoring_error)?;

    if persist && output.success && !output.diff.is_empty() {
        let json = graph
            .to_canonical_json(domain.schema_registry())
            .map_err(authoring_error)?;
        replace_file_contents(graph_path, &json).map_err(|source| CliError::Persist { source })?;
    }
    Ok(CliRunResult::diagnostics(to_json(&output)?, !output.success))
}

fn graph_view_inspect(graph_path: &Path, view_path: &Path) -> Result<CliRunResult, CliError> {
    let _graph: Graph = load_json(graph_path)?;
    let view: GraphView = load_json(view_path)?;
    let output = GraphViewAuthoringService::new()
        .inspect(&view, &read_permissions())
        .map_err(authoring_error)?;
    Ok(CliRunResult::success(to_json(&output)?))
}

fn graph_view_validate(graph_path: &Path, view_path: &Path) -> Result<CliRunResult, CliError> {
    let graph: Graph = load_json(graph_path)?;
    let view: GraphView = load_json(view_path)?;
    let output = GraphViewAuthoringService::new()
        .validate(&graph, &view, &read_permissions())
        .map_err(authoring_error)?;
    Ok(CliRunResult::diagnostics(
        to_json(&output)?,
        !output.success,
    ))
}

fn graph_view_mutate(
    graph_path: &Path,
    view_path: &Path,
    commands_path: &Path,
    persist: bool,
) -> Result<CliRunResult, CliError> {
    let graph: Graph = load_json(graph_path)?;
    let mut view: GraphView = load_json(view_path)?;
    let commands: Vec<GraphViewCommand> = load_json(commands_path)?;
    let service = GraphViewAuthoringService::new();
    let permissions = writable_permissions();
    let base = service
        .inspect(&view, &permissions)
        .map_err(authoring_error)?;
    let output = if persist {
        service.apply(
            &graph,
            &mut view,
            &permissions,
            base.revision,
            base.generation,
            commands,
        )
    } else {
        service.preview(
            &graph,
            &view,
            &permissions,
            base.revision,
            base.generation,
            commands,
        )
    }
    .map_err(authoring_error)?;

    if persist && output.success && !output.diff.is_empty() {
        let json = view.to_canonical_json(&graph).map_err(authoring_error)?;
        replace_file_contents(view_path, &json).map_err(|source| CliError::Persist { source })?;
    }
    Ok(CliRunResult::diagnostics(to_json(&output)?, !output.success))
}

fn ui_inspect(path: &Path) -> Result<CliRunResult, CliError> {
    let session = UiAuthoringSession::new(load_ui(path)?);
    let output = UiAuthoringService::new()
        .inspect(&session, &read_permissions())
        .map_err(authoring_error)?;
    Ok(CliRunResult::success(to_json(&output)?))
}

fn ui_validate(path: &Path) -> Result<CliRunResult, CliError> {
    let session = UiAuthoringSession::new(load_ui(path)?);
    let output = UiAuthoringService::new()
        .validate(&session, &read_permissions())
        .map_err(authoring_error)?;
    Ok(CliRunResult::diagnostics(
        to_json(&output)?,
        !output.success,
    ))
}

fn ui_mutate(path: &Path, commands_path: &Path, persist: bool) -> Result<CliRunResult, CliError> {
    let mut session = UiAuthoringSession::new(load_ui(path)?);
    let commands: Vec<UiDocumentCommand> = load_json(commands_path)?;
    let service = UiAuthoringService::new();
    let permissions = writable_permissions();
    let base = service
        .inspect(&session, &permissions)
        .map_err(authoring_error)?;
    let output = if persist {
        service.apply(
            &mut session,
            &permissions,
            base.revision,
            base.generation,
            commands,
        )
    } else {
        service.preview(
            &session,
            &permissions,
            base.revision,
            base.generation,
            commands,
        )
    }
    .map_err(authoring_error)?;

    if persist && output.success && !output.diff.is_empty() {
        let json = session.document().to_json_string().map_err(CliError::Json)?;
        replace_file_contents(path, &json).map_err(|source| CliError::Persist { source })?;
    }
    Ok(CliRunResult::diagnostics(to_json(&output)?, !output.success))
}

fn timeline_inspect(path: &Path) -> Result<CliRunResult, CliError> {
    let session = TimelineAuthoringService::new(load_timeline(path).map_err(authoring_error)?)
        .map_err(authoring_error)?;
    let output = session
        .inspect(&read_permissions())
        .map_err(authoring_error)?;
    Ok(CliRunResult::success(to_json(&output)?))
}

fn timeline_validate(path: &Path) -> Result<CliRunResult, CliError> {
    let session = TimelineAuthoringService::new(load_timeline(path).map_err(authoring_error)?)
        .map_err(authoring_error)?;
    let output = session
        .validate(&read_permissions())
        .map_err(authoring_error)?;
    Ok(CliRunResult::diagnostics(
        to_json(&output)?,
        !output.success,
    ))
}

fn timeline_mutate(
    path: &Path,
    commands_path: &Path,
    persist: bool,
) -> Result<CliRunResult, CliError> {
    let mut session = TimelineAuthoringService::new(load_timeline(path).map_err(authoring_error)?)
        .map_err(authoring_error)?;
    let commands: Vec<TimelineAuthoringCommand> = load_json(commands_path)?;
    let permissions = writable_permissions();
    let base = session.inspect(&permissions).map_err(authoring_error)?;
    let output = if persist {
        session.apply_commands(
            &permissions,
            base.revision,
            base.generation,
            commands,
        )
    } else {
        session.preview_commands(
            &permissions,
            base.revision,
            base.generation,
            commands,
        )
    }
    .map_err(authoring_error)?;

    if persist && output.success && !output.diff.is_empty() {
        session.save(path).map_err(authoring_error)?;
    }
    Ok(CliRunResult::diagnostics(to_json(&output)?, !output.success))
}

fn read_permissions() -> AuthoringPermissions {
    AuthoringPermissions::read_only()
}

fn writable_permissions() -> AuthoringPermissions {
    AuthoringPermissions::read_only()
        .with(AuthoringPermission::Preview)
        .with(AuthoringPermission::ProjectDataWrite)
}

fn load_ui(path: &Path) -> Result<UiDocument, CliError> {
    let json = fs::read_to_string(path).map_err(|source| CliError::Io {
        path: path.to_owned(),
        source,
    })?;
    UiDocument::from_json_str(&json).map_err(|error| CliError::Authoring {
        diagnostics: vec![Diagnostic::error("ui.load_failed", error.to_string())],
    })
}

fn load_json<T: DeserializeOwned>(path: &Path) -> Result<T, CliError> {
    let json = fs::read_to_string(path).map_err(|source| CliError::Io {
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_str(&json).map_err(|source| CliError::InvalidJson {
        path: path.to_owned(),
        source,
    })
}

fn authoring_error(error: impl std::fmt::Display) -> CliError {
    CliError::Authoring {
        diagnostics: vec![Diagnostic::error(
            "authoring.adapter_error",
            error.to_string(),
        )],
    }
}
