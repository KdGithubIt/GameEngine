use engine_mcp_host::transport::{McpHostResult, McpServer, McpServerInfo, MCP_PROTOCOL_VERSION};
use engine_mcp_host::{HeadlessAuthoringHost, HeadlessProjectSelection};
use serde_json::json;
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let options = Options::parse(env::args().skip(1))?;
    let selection = HeadlessProjectSelection {
        scene: options.scene,
        graph: options.graph,
        graph_view: options.graph_view,
        ui: options.ui,
        material: options.material,
        animation_set: options.animation_set,
    };
    let mut host = if options.read_only {
        HeadlessAuthoringHost::open_read_only(&options.project, selection)
    } else {
        HeadlessAuthoringHost::open_writer(&options.project, selection)
    }
    .map_err(|error| error.to_string())?;

    let view = host.view_descriptor();
    let instructions = if view.writable {
        "Authoritative headless project writer. State starts from canonical saved files and successful project-data mutations are persisted before the next request. Live Editor unsaved state is never visible."
    } else {
        "Read-only headless saved-file snapshot. Live Editor unsaved state is never visible; mutation tools remain advertised for parity but permission checks reject commits."
    };
    let (server, requests) = McpServer::start(
        McpServerInfo::new("gameengine-headless-authoring", instructions),
        || {},
    )
    .map_err(|error| error.to_string())?;

    println!(
        "{}",
        serde_json::to_string(&json!({
            "endpoint": server.endpoint(),
            "authorization_token": server.authorization_token(),
            "protocol_version": MCP_PROTOCOL_VERSION,
            "view": view,
        }))
        .map_err(|error| error.to_string())?
    );

    for request in requests {
        let result = match host.handle_tool_call(request.name(), request.arguments().clone()) {
            Ok(value) => McpHostResult::Success(value),
            Err(error) => McpHostResult::ToolError {
                code: error.code().to_owned(),
                message: error.message().to_owned(),
            },
        };
        request.respond(result);
    }
    drop(server);
    Ok(())
}

struct Options {
    project: PathBuf,
    read_only: bool,
    scene: Option<String>,
    graph: Option<String>,
    graph_view: Option<String>,
    ui: Option<String>,
    material: Option<String>,
    animation_set: Option<String>,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut arguments = arguments.into_iter();
        let mut project = None;
        let mut read_only = false;
        let mut scene = None;
        let mut graph = None;
        let mut graph_view = None;
        let mut ui = None;
        let mut material = None;
        let mut animation_set = None;

        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--project" => project = Some(PathBuf::from(require_value(&mut arguments, "--project")?)),
                "--read-only" => read_only = true,
                "--scene" => scene = Some(require_value(&mut arguments, "--scene")?),
                "--graph" => graph = Some(require_value(&mut arguments, "--graph")?),
                "--graph-view" => graph_view = Some(require_value(&mut arguments, "--graph-view")?),
                "--ui" => ui = Some(require_value(&mut arguments, "--ui")?),
                "--material" => material = Some(require_value(&mut arguments, "--material")?),
                "--animation-set" => animation_set = Some(require_value(&mut arguments, "--animation-set")?),
                "--help" | "-h" => return Err(usage()),
                other => return Err(format!("unknown argument `{other}`\n{}", usage())),
            }
        }

        let project = project.ok_or_else(|| format!("--project is required\n{}", usage()))?;
        Ok(Self { project, read_only, scene, graph, graph_view, ui, material, animation_set })
    }
}

fn require_value(arguments: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    arguments.next().ok_or_else(|| format!("{flag} requires a value"))
}

fn usage() -> String {
    "usage: engine-mcp-host --project <path> [--read-only] [--scene <asset-relative>] [--graph <asset-relative>] [--graph-view <asset-relative>] [--ui <asset-relative>] [--material <asset-relative>] [--animation-set <asset-relative>]".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_is_default_and_read_only_is_explicit() {
        let writer = Options::parse(["--project".into(), "demo".into()]).expect("writer options");
        assert!(!writer.read_only);
        let read_only = Options::parse(["--project".into(), "demo".into(), "--read-only".into()]).expect("read-only options");
        assert!(read_only.read_only);
    }
}
