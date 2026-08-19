//! Standalone loopback host for narrow remote Launcher/Editor lifecycle control.

use engine_launcher::remote_lifecycle::{
    DEFAULT_REMOTE_LIFECYCLE_PORT, REMOTE_LIFECYCLE_TOKEN_ENV, RemoteLifecycleHostConfig,
    serve_remote_lifecycle_host,
};
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
    let bearer_token = std::env::var(REMOTE_LIFECYCLE_TOKEN_ENV).map_err(|_| {
        format!(
            "{REMOTE_LIFECYCLE_TOKEN_ENV} must be set to a dedicated lifecycle credential"
        )
    })?;
    let mut port = DEFAULT_REMOTE_LIFECYCLE_PORT;
    let mut allow_editor_start = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--allow-editor-start" => allow_editor_start = true,
            "--port" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--port requires a TCP port".to_owned())?;
                port = value
                    .parse::<u16>()
                    .map_err(|_| "--port must be an unsigned 16-bit TCP port".to_owned())?;
            }
            "--help" | "-h" => {
                println!(
                    "engine-remote-lifecycle-host [--port <PORT>] [--allow-editor-start]\n\
                     \n\
                     The host always binds to loopback. Set {REMOTE_LIFECYCLE_TOKEN_ENV} to a \
                     dedicated bearer credential before starting it."
                );
                return Ok(());
            }
            _ => return Err(format!("unsupported lifecycle-host argument: {argument}")),
        }
    }

    let config = RemoteLifecycleHostConfig::new(bearer_token)
        .with_port(port)
        .with_editor_start_permission(allow_editor_start);
    serve_remote_lifecycle_host(config).map_err(|error| error.to_string())
}
