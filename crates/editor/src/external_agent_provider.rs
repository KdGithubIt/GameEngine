//! Provider-specific adapters for external coding-agent runtimes.
//!
//! Adapters translate provider lifecycle and wire-protocol details into the
//! existing Agent Host boundary. They do not own authoring semantics,
//! permissions, code apply, validation, or completion gates.

use crate::agent_host::terminate_process_tree;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::io;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const GAMEENGINE_AGENT_EVENT_PREFIX: &str = "GAMEENGINE_AGENT_EVENT ";
const GAMEENGINE_MCP_SERVER_NAME: &str = "gameengine_editor";
const GAMEENGINE_MCP_TOKEN_ENV: &str = "GAMEENGINE_MCP_AUTH_TOKEN";
const GAMEENGINE_AGENT_RUN_ID_ENV: &str = "GAMEENGINE_AGENT_RUN_ID";
const GAMEENGINE_AGENT_RUN_ID_HEADER: &str = "X-GameEngine-Agent-Run-Id";
const PROVIDER_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const CLAUDE_CODE_VERSION: &str = "2.1.237";
const CODEX_VERSION: &str = "0.148.0";
const CODEX_MCP_FEATURE: &str = "mcp_2026_07_28";
const MCP_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);

/// Where an external agent provider process runs.
///
/// Provider CLIs are commonly installed in a Linux userland on a Windows
/// workstation. The environment is a launch concern only: it changes how the
/// process is started and how variables and paths cross the boundary, never
/// what the provider is allowed to do, which stays owned by the Agent Host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalAgentExecutionEnvironment {
    #[default]
    WindowsNative,
    Wsl2Linux,
}

impl ExternalAgentExecutionEnvironment {
    pub(crate) const ALL: [Self; 2] = [Self::WindowsNative, Self::Wsl2Linux];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::WindowsNative => "Windows native",
            Self::Wsl2Linux => "WSL2 Linux",
        }
    }
}

/// How one external agent launch is placed on this machine.
#[derive(Debug, Clone, Default)]
pub(crate) struct ExternalAgentExecutionPlacement {
    pub(crate) environment: ExternalAgentExecutionEnvironment,
    /// WSL distribution name, or empty for the user's default distribution.
    pub(crate) distribution: String,
}

impl ExternalAgentExecutionPlacement {
    #[cfg(test)]
    pub(crate) fn windows_native() -> Self {
        Self::default()
    }

    fn wsl_prefix_args(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        let distribution = self.distribution.trim();
        if !distribution.is_empty() {
            args.push(OsString::from("-d"));
            args.push(OsString::from(distribution));
        }
        args
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalAgentProviderKind {
    ClaudeCode,
    Codex,
    #[default]
    Generic,
}

impl ExternalAgentProviderKind {
    pub(crate) const ALL: [Self; 3] = [Self::ClaudeCode, Self::Codex, Self::Generic];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Generic => "Generic command",
        }
    }

    pub(crate) fn run_label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Generic => "generic-external",
        }
    }

    fn program(self) -> Option<&'static OsStr> {
        match self {
            Self::ClaudeCode => Some(OsStr::new("claude")),
            Self::Codex => Some(OsStr::new("codex")),
            Self::Generic => None,
        }
    }

    /// Whether this provider can answer an Ask turn under a read-only launch.
    ///
    /// Only a first-class adapter qualifies: GameEngine knows the exact
    /// argument vector that keeps that provider from writing, while a generic
    /// command is user-defined and carries no such guarantee.
    pub(crate) const fn can_answer_questions(self) -> bool {
        matches!(self, Self::ClaudeCode | Self::Codex)
    }

    /// Whether this provider owns an interactive sign-in the Editor can start.
    pub(crate) const fn can_sign_in(self) -> bool {
        matches!(self, Self::ClaudeCode | Self::Codex)
    }

    pub(crate) fn capabilities(self) -> ExternalAgentProviderCapabilities {
        match self {
            Self::ClaudeCode | Self::Codex => ExternalAgentProviderCapabilities {
                provider_managed_auth: true,
                mcp_injection: true,
                structured_events: true,
                host_cancellation: true,
            },
            Self::Generic => ExternalAgentProviderCapabilities {
                provider_managed_auth: false,
                mcp_injection: true,
                structured_events: true,
                host_cancellation: true,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExternalAgentProviderCapabilities {
    pub(crate) provider_managed_auth: bool,
    pub(crate) mcp_injection: bool,
    pub(crate) structured_events: bool,
    pub(crate) host_cancellation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalAgentDiscoveryStatus {
    Unchecked,
    Available,
    Unavailable,
}

impl ExternalAgentDiscoveryStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Unchecked => "not checked",
            Self::Available => "available",
            Self::Unavailable => "not found",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalAgentAuthStatus {
    Unchecked,
    Authenticated,
    SignInRequired,
    NotApplicable,
    Unavailable,
}

impl ExternalAgentAuthStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Unchecked => "not checked",
            Self::Authenticated => "authenticated",
            Self::SignInRequired => "sign-in required",
            Self::NotApplicable => "provider-managed status unavailable",
            Self::Unavailable => "provider unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalAgentProviderStatus {
    pub(crate) kind: ExternalAgentProviderKind,
    pub(crate) discovery: ExternalAgentDiscoveryStatus,
    pub(crate) auth: ExternalAgentAuthStatus,
}

impl ExternalAgentProviderStatus {
    pub(crate) fn unchecked(kind: ExternalAgentProviderKind) -> Self {
        let auth = if kind == ExternalAgentProviderKind::Generic {
            ExternalAgentAuthStatus::NotApplicable
        } else {
            ExternalAgentAuthStatus::Unchecked
        };
        Self {
            kind,
            discovery: ExternalAgentDiscoveryStatus::Unchecked,
            auth,
        }
    }

    pub(crate) fn generic(configured: bool) -> Self {
        Self {
            kind: ExternalAgentProviderKind::Generic,
            discovery: if configured {
                ExternalAgentDiscoveryStatus::Available
            } else {
                ExternalAgentDiscoveryStatus::Unavailable
            },
            auth: ExternalAgentAuthStatus::NotApplicable,
        }
    }

    #[cfg(feature = "visual-validation")]
    pub(crate) fn visual_fixture(kind: ExternalAgentProviderKind) -> Self {
        Self {
            kind,
            discovery: ExternalAgentDiscoveryStatus::Available,
            auth: if kind == ExternalAgentProviderKind::Generic {
                ExternalAgentAuthStatus::NotApplicable
            } else {
                ExternalAgentAuthStatus::Authenticated
            },
        }
    }

    pub(crate) fn ready(&self) -> bool {
        self.discovery == ExternalAgentDiscoveryStatus::Available
            && matches!(
                self.auth,
                ExternalAgentAuthStatus::Authenticated | ExternalAgentAuthStatus::NotApplicable
            )
    }

    pub(crate) fn remote_json(&self) -> Value {
        let capabilities = self.kind.capabilities();
        serde_json::json!({
            "kind": self.kind.run_label(),
            "discovery": self.discovery.label(),
            "authentication": self.auth.label(),
            "capabilities": {
                "provider_managed_auth": capabilities.provider_managed_auth,
                "mcp_injection": capabilities.mcp_injection,
                "structured_events": capabilities.structured_events,
                "host_cancellation": capabilities.host_cancellation,
            }
        })
    }
}

pub(crate) fn probe_provider(
    kind: ExternalAgentProviderKind,
    generic_program: &str,
    placement: &ExternalAgentExecutionPlacement,
) -> ExternalAgentProviderStatus {
    if kind == ExternalAgentProviderKind::Generic {
        return ExternalAgentProviderStatus::generic(!generic_program.trim().is_empty());
    }
    let Some(program) = kind.program() else {
        return ExternalAgentProviderStatus::unchecked(kind);
    };
    let version = command_output(placement, program, ["--version"]);
    let available = version
        .as_ref()
        .is_ok_and(|(succeeded, output)| *succeeded && provider_version_matches(kind, output));
    if !available {
        return ExternalAgentProviderStatus {
            kind,
            discovery: ExternalAgentDiscoveryStatus::Unavailable,
            auth: ExternalAgentAuthStatus::Unavailable,
        };
    }
    let auth = match kind {
        ExternalAgentProviderKind::ClaudeCode => {
            command_output(placement, program, ["auth", "status"])
                .map(|(succeeded, output)| claude_credential_present(&output, succeeded))
        }
        ExternalAgentProviderKind::Codex => {
            command_success(placement, program, ["login", "status"])
        }
        ExternalAgentProviderKind::Generic => Ok(true),
    };
    ExternalAgentProviderStatus {
        kind,
        discovery: ExternalAgentDiscoveryStatus::Available,
        auth: match auth {
            Ok(true) => ExternalAgentAuthStatus::Authenticated,
            Ok(false) => ExternalAgentAuthStatus::SignInRequired,
            Err(_) => ExternalAgentAuthStatus::SignInRequired,
        },
    }
}

fn command_success<I, S>(
    placement: &ExternalAgentExecutionPlacement,
    program: &OsStr,
    args: I,
) -> io::Result<bool>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let (program, args) = placed_launch_command(
        placement,
        program.to_os_string(),
        args.into_iter()
            .map(|argument| argument.as_ref().to_os_string())
            .collect(),
    );
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_for_probe(&mut child).map(|status| status.success())
}

fn direct_command_output(program: OsString, args: Vec<OsString>) -> io::Result<(bool, String)> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let status = wait_for_probe(&mut child)?;
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_string(&mut output)?;
    }
    Ok((status.success(), output))
}

/// Runs one command for its exit status and captured standard output.
pub(crate) fn command_output<I, S>(
    placement: &ExternalAgentExecutionPlacement,
    program: &OsStr,
    args: I,
) -> io::Result<(bool, String)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let (program, args) = placed_launch_command(
        placement,
        program.to_os_string(),
        args.into_iter()
            .map(|argument| argument.as_ref().to_os_string())
            .collect(),
    );
    direct_command_output(program, args)
}

fn wait_for_probe(child: &mut Child) -> io::Result<ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= PROVIDER_PROBE_TIMEOUT {
            let _ = terminate_process_tree(child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "provider probe exceeded its 10 second budget",
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn provider_version_matches(kind: ExternalAgentProviderKind, output: &str) -> bool {
    let expected = match kind {
        ExternalAgentProviderKind::ClaudeCode => CLAUDE_CODE_VERSION,
        ExternalAgentProviderKind::Codex => CODEX_VERSION,
        ExternalAgentProviderKind::Generic => return true,
    };
    output
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '.'))
        .any(|token| token == expected)
}

/// Reads whether Claude Code holds a credential from its status report.
///
/// `claude auth status` reports a signed-out session in its JSON body and still
/// exits successfully. An unknown response is not accepted as authenticated:
/// the pinned adapter must understand the credential report before Build or Ask
/// can send project evidence to the provider.
pub(crate) fn claude_credential_present(output: &str, _exit_succeeded: bool) -> bool {
    serde_json::from_str::<Value>(output)
        .ok()
        .and_then(|status| status.get("loggedIn").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// Rewrites one command for the environment it must run in.
///
/// A Windows-native placement runs the provider directly. A WSL2 placement runs
/// the same provider argument vector through `wsl.exe`, which passes it to the
/// Linux binary without an intervening shell, so provider arguments are never
/// re-quoted or word-split on the way in.
fn placed_command<I, S>(
    placement: &ExternalAgentExecutionPlacement,
    program: OsString,
    args: I,
) -> (OsString, Vec<OsString>)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let provider_args = args
        .into_iter()
        .map(|argument| argument.as_ref().to_os_string())
        .collect::<Vec<_>>();
    match placement.environment {
        ExternalAgentExecutionEnvironment::WindowsNative => (program, provider_args),
        ExternalAgentExecutionEnvironment::Wsl2Linux => {
            let mut wrapped = placement.wsl_prefix_args();
            wrapped.push(OsString::from("--"));
            wrapped.push(program);
            wrapped.extend(provider_args);
            (OsString::from(WSL_LAUNCHER), wrapped)
        }
    }
}

/// Extensions Windows process creation can start from a bare program name.
///
/// A bare name is completed with `.exe` only. A provider CLI installed by npm
/// is reached through a `.cmd` shim instead, so launching the bare name fails
/// with "program not found" while the provider is installed and working, and
/// the Editor then reports a finished install as missing.
const WINDOWS_LAUNCHER_EXTENSIONS: [&str; 3] = ["exe", "cmd", "bat"];

/// Launcher extensions Windows starts through the command processor.
///
/// `cmd.exe` receives one command line and parses it as a line of script, so an
/// argument containing a line break cannot be represented at all: process
/// creation rejects the whole launch before the provider runs. A provider
/// prompt is multi-line, which makes this the difference between a working and
/// an impossible launch rather than a quoting detail.
const WINDOWS_BATCH_LAUNCHER_EXTENSIONS: [&str; 2] = ["cmd", "bat"];

/// One program this machine can start, and the arguments it owns.
///
/// A provider name can resolve to an interpreter that runs the provider's own
/// script, so a resolved launcher is not always just a program: the script path
/// belongs to the launcher and must precede the arguments the caller built.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedLauncher {
    program: OsString,
    leading_args: Vec<OsString>,
}

impl ResolvedLauncher {
    /// A launcher that is started with the caller's arguments alone.
    fn direct(program: OsString) -> Self {
        Self {
            program,
            leading_args: Vec::new(),
        }
    }

    /// Places caller arguments after the arguments the launcher owns.
    fn with_arguments(self, args: Vec<OsString>) -> (OsString, Vec<OsString>) {
        let mut placed = self.leading_args;
        placed.extend(args);
        (self.program, placed)
    }
}

/// Rewrites one provider name into a launcher this environment can start.
///
/// Only a Windows-native launch needs this. A WSL launch resolves the name
/// inside the distribution, where an extension is not part of a program name.
fn resolve_launcher(
    placement: &ExternalAgentExecutionPlacement,
    program: OsString,
) -> ResolvedLauncher {
    match placement.environment {
        ExternalAgentExecutionEnvironment::WindowsNative => resolve_windows_launcher(
            &program,
            &search_path_directories(),
            &|candidate| candidate.is_file(),
            &|candidate| std::fs::read_to_string(candidate).ok(),
        )
        .unwrap_or_else(|| ResolvedLauncher::direct(program)),
        ExternalAgentExecutionEnvironment::Wsl2Linux => ResolvedLauncher::direct(program),
    }
}

/// Builds the command one launch plan runs, for this machine and placement.
///
/// Launcher resolution happens before placement, so the WSL wrapper receives
/// the same argument vector the provider will see.
pub(crate) fn placed_launch_command(
    placement: &ExternalAgentExecutionPlacement,
    program: OsString,
    args: Vec<OsString>,
) -> (OsString, Vec<OsString>) {
    let (program, args) = resolve_launcher(placement, program).with_arguments(args);
    placed_command(placement, program, args)
}

/// The `PATH` directories a Windows-native launch searches, in order.
fn search_path_directories() -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .filter(|directory| !directory.as_os_str().is_empty())
        .collect()
}

/// Finds the first launchable file one bare program name resolves to.
///
/// A resolved batch shim is unwrapped to the launcher it forwards to, because a
/// shim cannot carry a multi-line prompt. Returns `None` for a name that
/// already carries an extension or a directory component: that name is what the
/// installer or the user asked for, and process creation can start it as
/// written.
fn resolve_windows_launcher(
    program: &OsStr,
    directories: &[PathBuf],
    is_file: &dyn Fn(&Path) -> bool,
    read_text: &dyn Fn(&Path) -> Option<String>,
) -> Option<ResolvedLauncher> {
    let name = Path::new(program);
    let is_bare = name
        .parent()
        .is_some_and(|parent| parent.as_os_str().is_empty());
    if !is_bare || name.extension().is_some() {
        return None;
    }
    directories.iter().find_map(|directory| {
        WINDOWS_LAUNCHER_EXTENSIONS.iter().find_map(|extension| {
            let candidate = directory.join(name).with_extension(extension);
            if !is_file(&candidate) {
                return None;
            }
            Some(
                unwrap_windows_batch_shim(&candidate, is_file, read_text)
                    .unwrap_or_else(|| ResolvedLauncher::direct(candidate.into_os_string())),
            )
        })
    })
}

/// Reports whether a launcher runs through the Windows command processor.
fn is_windows_batch_launcher(program: &Path) -> bool {
    program
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            WINDOWS_BATCH_LAUNCHER_EXTENSIONS
                .iter()
                .any(|batch| extension.eq_ignore_ascii_case(batch))
        })
}

/// Marker a shim uses to forward every argument it received.
const WINDOWS_SHIM_ARGUMENT_FORWARD: &str = "%*";

/// Variable an npm shim assigns the interpreter it selected.
const WINDOWS_SHIM_INTERPRETER_VARIABLE: &str = "_prog";

/// Resolves a batch shim to the launcher it forwards the caller's arguments to.
///
/// A CLI installed by npm is placed on `PATH` as a `.cmd` shim. The shim cannot
/// receive a multi-line prompt (see [`WINDOWS_BATCH_LAUNCHER_EXTENSIONS`]),
/// while the program it forwards to receives arguments verbatim, so reading the
/// shim is what makes a prompt with line breaks reach the provider at all. Both
/// shapes npm writes are resolved: a package shipping a native binary forwards
/// to a sibling executable, and a package shipping a script forwards to an
/// interpreter that is handed that script.
///
/// A shim whose forwarding line this cannot fully account for is left to launch
/// as written, because starting a guessed program would run something the user
/// never asked for.
fn unwrap_windows_batch_shim(
    shim: &Path,
    is_file: &dyn Fn(&Path) -> bool,
    read_text: &dyn Fn(&Path) -> Option<String>,
) -> Option<ResolvedLauncher> {
    if !is_windows_batch_launcher(shim) {
        return None;
    }
    let directory = shim.parent()?;
    let text = read_text(shim)?;
    let tokens = shim_forwarding_tokens(&text)?;
    let (target, launcher_args) = tokens.split_first()?;
    let program = resolve_shim_target(target, &text, directory, is_file)?;
    let mut leading_args = Vec::new();
    for argument in launcher_args {
        if argument == WINDOWS_SHIM_ARGUMENT_FORWARD {
            break;
        }
        let expanded = expand_shim_own_directory(argument, directory)?;
        if names_shim_own_directory(argument) && !is_file(&expanded) {
            return None;
        }
        leading_args.push(expanded.into_os_string());
    }
    Some(ResolvedLauncher {
        program,
        leading_args,
    })
}

/// Splits the shim line that starts the real program into its tokens.
///
/// `%*` forwards every argument the shim received, so the last line carrying it
/// is the line that starts the program. That line can chain command processor
/// bookkeeping in front of the launch, and only the final `&` clause is the
/// launch itself.
fn shim_forwarding_tokens(text: &str) -> Option<Vec<String>> {
    let launch = text
        .lines()
        .rev()
        .find(|line| line.contains(WINDOWS_SHIM_ARGUMENT_FORWARD))
        .and_then(|line| line.rsplit('&').next())?;
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut inside_quotes = false;
    for character in launch.chars() {
        match character {
            '"' => inside_quotes = !inside_quotes,
            character if character.is_whitespace() && !inside_quotes => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            character => token.push(character),
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    (!tokens.is_empty()).then_some(tokens)
}

/// Resolves the program token of a shim's forwarding line to a launcher.
///
/// The token is either the program itself or the variable the shim assigned the
/// interpreter it selected. An interpreter the shim resolves through `PATH` is
/// kept as a bare name, because that is the program the shim would have run.
fn resolve_shim_target(
    target: &str,
    text: &str,
    directory: &Path,
    is_file: &dyn Fn(&Path) -> bool,
) -> Option<OsString> {
    let candidates = if target
        .trim_matches('%')
        .eq_ignore_ascii_case(WINDOWS_SHIM_INTERPRETER_VARIABLE)
    {
        shim_assigned_values(text, WINDOWS_SHIM_INTERPRETER_VARIABLE)
    } else {
        vec![target.to_owned()]
    };
    let mut bare_name = None;
    for candidate in candidates {
        let Some(expanded) = expand_shim_own_directory(&candidate, directory) else {
            continue;
        };
        if is_file(&expanded) {
            return Some(expanded.into_os_string());
        }
        if bare_name.is_none() && Path::new(&candidate).file_name() == Some(OsStr::new(&candidate))
        {
            bare_name = Some(OsString::from(candidate));
        }
    }
    bare_name
}

/// Collects the values one shim variable is assigned, in the order written.
fn shim_assigned_values(text: &str, variable: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let statement = line.trim().trim_start_matches(['(', ')']).trim();
            let (keyword, rest) = statement.split_once(char::is_whitespace)?;
            if !keyword.eq_ignore_ascii_case("SET") {
                return None;
            }
            let (name, value) = rest.trim().trim_matches('"').split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case(variable)
                .then(|| value.trim_matches('"').to_owned())
        })
        .collect()
}

/// Reports whether a shim token is written relative to the shim's directory.
fn names_shim_own_directory(token: &str) -> bool {
    token.contains("%~dp0") || token.contains("%dp0%")
}

/// Expands the variables a shim uses to name its own directory.
///
/// A shim refers to its neighbours through `%~dp0`, usually by way of a `dp0`
/// variable it sets first. A token holding any other variable is rejected rather
/// than guessed at, because this expansion decides what is run.
fn expand_shim_own_directory(token: &str, directory: &Path) -> Option<PathBuf> {
    let directory = directory.to_str()?;
    let expanded = token
        .replace("%~dp0", directory)
        .replace("%dp0%", directory);
    if expanded.contains('%') {
        return None;
    }
    Some(PathBuf::from(expanded))
}

/// Rejects a launch whose arguments the resolved launcher cannot carry.
///
/// # Errors
///
/// Returns an error when a batch shim would receive an argument containing a
/// line break. Windows rejects that command line in process creation, with a
/// message that names neither the argument nor the shim, so the condition is
/// reported here while the provider that caused it is still in view.
fn ensure_launcher_carries_arguments(program: &OsStr, args: &[OsString]) -> Result<(), String> {
    let launcher = Path::new(program);
    if !is_windows_batch_launcher(launcher) {
        return Ok(());
    }
    if !args
        .iter()
        .any(|argument| argument.to_string_lossy().contains(['\n', '\r']))
    {
        return Ok(());
    }
    Err(format!(
        "{} is a batch shim, and Windows cannot pass a multi-line prompt through one. Reinstall the provider so its own executable is on PATH, or run it in the WSL2 environment.",
        launcher.file_name().unwrap_or(program).to_string_lossy()
    ))
}

/// Windows launcher used to place a provider process inside a WSL distribution.
const WSL_LAUNCHER: &str = "wsl.exe";
/// Variable naming the Windows variables WSL forwards into the Linux session.
const WSL_ENVIRONMENT_FORWARD_VARIABLE: &str = "WSLENV";
/// Suffix marking a forwarded variable whose value is a path to translate.
const WSL_PATH_TRANSLATION_SUFFIX: &str = "/p";

/// Builds the `WSLENV` entry that forwards Editor variables into WSL.
///
/// Windows environment variables do not reach a Linux process unless they are
/// named here, so an unlisted variable silently disappears. Variables whose
/// value is a Windows path are marked for translation so the provider reads the
/// same file the Editor wrote.
pub(crate) fn wsl_environment_forwarding(
    variables: &[(OsString, OsString)],
    path_variables: &[&str],
) -> (OsString, OsString) {
    let mut forwarded = std::env::var_os(WSL_ENVIRONMENT_FORWARD_VARIABLE)
        .map(|value| {
            value
                .to_string_lossy()
                .split(':')
                .filter(|entry| !entry.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for entry in variables
        .iter()
        .filter_map(|(name, _)| name.to_str())
        .map(|name| {
            if path_variables.contains(&name) {
                format!("{name}{WSL_PATH_TRANSLATION_SUFFIX}")
            } else {
                name.to_owned()
            }
        })
    {
        let forwarded_name = entry.split('/').next().unwrap_or(&entry);
        forwarded.retain(|existing| existing.split('/').next() != Some(forwarded_name));
        forwarded.push(entry);
    }
    (
        OsString::from(WSL_ENVIRONMENT_FORWARD_VARIABLE),
        OsString::from(forwarded.join(":")),
    )
}

/// Checks that a WSL2 session can reach the Editor loopback endpoint.
///
/// ADR 0121 keeps the Editor MCP endpoint bound to loopback. A WSL2 session
/// reaches that endpoint only when the distribution shares the host loopback,
/// so this proves reachability before a run starts instead of letting the
/// provider fail mid-turn with a connection error it cannot explain.
pub(crate) fn probe_wsl_loopback_reachability(
    placement: &ExternalAgentExecutionPlacement,
    mcp_endpoint: &str,
    authorization_token: &str,
) -> Result<(), String> {
    if placement.environment != ExternalAgentExecutionEnvironment::Wsl2Linux {
        return Ok(());
    }
    let (host, port) = loopback_authority(mcp_endpoint)?;
    let mut args = placement.wsl_prefix_args();
    args.push(OsString::from("--"));
    args.push(OsString::from("sh"));
    args.push(OsString::from("-lc"));
    args.push(OsString::from(
        r#"if ! command -v curl >/dev/null 2>&1; then exit 42; fi
response=$(curl --silent --show-error --fail --max-time 10 \
  -H "Authorization: Bearer $GAMEENGINE_MCP_AUTH_TOKEN" \
  -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"gameengine-wsl-probe","version":"1"}}}' \
  "$GAMEENGINE_MCP_ENDPOINT") || exit $?
printf '%s' "$response" | grep -q '"protocolVersion"'"#,
    ));
    let status = Command::new(WSL_LAUNCHER)
        .args(args)
        .env(GAMEENGINE_MCP_TOKEN_ENV, authorization_token)
        .env("GAMEENGINE_MCP_ENDPOINT", mcp_endpoint)
        .env(
            WSL_ENVIRONMENT_FORWARD_VARIABLE,
            format!("{GAMEENGINE_MCP_TOKEN_ENV}:GAMEENGINE_MCP_ENDPOINT"),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|mut child| wait_for_probe(&mut child))
        .map_err(|error| format!("could not run {WSL_LAUNCHER}: {error}"))?;
    if status.success() || status.code() == Some(42) {
        return Ok(());
    }
    Err(format!(
        "the WSL2 distribution could not complete an authenticated MCP handshake with the Editor at {host}:{port}. Enable mirrored networking for WSL (networkingMode=mirrored in .wslconfig), verify the Editor MCP endpoint, or run the provider in the Windows-native environment."
    ))
}

/// Performs an authenticated legacy MCP initialize request from the Editor
/// process before a write-capable external Build is launched.
///
/// Provider discovery and authentication do not prove that the provider can
/// reach the project-scoped Editor MCP endpoint. This probe closes that gap
/// for the Windows-native path, where the previous WSL-only probe was a no-op.
/// The request deliberately uses `initialize` rather than a mutating tool, so
/// the probe cannot dirty the project or acquire authoring ownership.
pub(crate) fn probe_editor_mcp_endpoint(
    mcp_endpoint: &str,
    authorization_token: &str,
) -> Result<(), String> {
    if !mcp_endpoint.ends_with("/mcp") {
        return Err("Editor MCP endpoint must use the /mcp path".to_owned());
    }
    let (host, port) = loopback_authority(mcp_endpoint)?;
    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]") {
        return Err("Editor MCP endpoint must remain loopback-only".to_owned());
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| "Editor MCP endpoint has an invalid port".to_owned())?;
    let address = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|error| format!("could not resolve Editor MCP endpoint: {error}"))?
        .next()
        .ok_or_else(|| "Editor MCP endpoint did not resolve to an address".to_owned())?;
    let mut stream = TcpStream::connect_timeout(&address, MCP_PREFLIGHT_TIMEOUT)
        .map_err(|error| format!("could not reach Editor MCP endpoint: {error}"))?;
    stream
        .set_read_timeout(Some(MCP_PREFLIGHT_TIMEOUT))
        .map_err(|error| format!("could not configure Editor MCP probe: {error}"))?;
    stream
        .set_write_timeout(Some(MCP_PREFLIGHT_TIMEOUT))
        .map_err(|error| format!("could not configure Editor MCP probe: {error}"))?;

    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"gameengine-preflight","version":"1"}}}"#;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {authorization_token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.flush())
        .map_err(|error| format!("could not send Editor MCP preflight: {error}"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| format!("could not read Editor MCP preflight: {error}"))?;
    let response = String::from_utf8_lossy(&response);
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "Editor MCP preflight returned an invalid HTTP response".to_owned())?;
    let status_line = headers
        .lines()
        .next()
        .ok_or_else(|| "Editor MCP preflight returned no HTTP status".to_owned())?;
    if !status_line.starts_with("HTTP/1.1 200 ") {
        return Err(format!(
            "Editor MCP preflight returned HTTP status {}",
            status_line.split_whitespace().nth(1).unwrap_or("unknown")
        ));
    }
    let payload: Value = serde_json::from_str(body)
        .map_err(|_| "Editor MCP preflight returned invalid JSON".to_owned())?;
    if payload
        .get("result")
        .and_then(|result| result.get("protocolVersion"))
        .and_then(Value::as_str)
        .is_none()
    {
        return Err("Editor MCP preflight did not return an MCP initialize result".to_owned());
    }
    Ok(())
}

/// Splits a loopback HTTP endpoint into its host and port.
fn loopback_authority(mcp_endpoint: &str) -> Result<(String, String), String> {
    let authority = mcp_endpoint
        .trim()
        .strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .ok_or_else(|| "the Editor MCP endpoint is not a loopback HTTP endpoint".to_owned())?;
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| "the Editor MCP endpoint does not carry a port".to_owned())?;
    Ok((host.to_owned(), port.to_owned()))
}

#[derive(Debug, Clone)]
pub(crate) struct ExternalAgentLaunchPlan {
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
}

pub(crate) fn build_launch_plan(
    kind: ExternalAgentProviderKind,
    placement: &ExternalAgentExecutionPlacement,
    generic_program: &str,
    generic_args: &[String],
    prompt: &str,
    mcp_endpoint: &str,
) -> Result<ExternalAgentLaunchPlan, String> {
    let plan = build_provider_launch_plan(
        kind,
        placement,
        generic_program,
        generic_args,
        prompt,
        mcp_endpoint,
    )?;
    let (program, args) = placed_launch_command(placement, plan.program, plan.args);
    ensure_launcher_carries_arguments(&program, &args)?;
    Ok(ExternalAgentLaunchPlan { program, args })
}

fn build_provider_launch_plan(
    kind: ExternalAgentProviderKind,
    placement: &ExternalAgentExecutionPlacement,
    generic_program: &str,
    generic_args: &[String],
    prompt: &str,
    mcp_endpoint: &str,
) -> Result<ExternalAgentLaunchPlan, String> {
    match kind {
        ExternalAgentProviderKind::ClaudeCode => {
            let server = serde_json::json!({
                "type": "http",
                "url": mcp_endpoint,
                "headers": {
                    "Authorization": format!("Bearer ${{{GAMEENGINE_MCP_TOKEN_ENV}}}"),
                    GAMEENGINE_AGENT_RUN_ID_HEADER: format!("${{{GAMEENGINE_AGENT_RUN_ID_ENV}}}"),
                },
            });
            let mut servers = serde_json::Map::new();
            servers.insert(GAMEENGINE_MCP_SERVER_NAME.to_owned(), server);
            let mut root = serde_json::Map::new();
            root.insert("mcpServers".to_owned(), Value::Object(servers));
            let mcp_config = Value::Object(root).to_string();
            Ok(ExternalAgentLaunchPlan {
                program: OsString::from("claude"),
                args: vec![
                    OsString::from("-p"),
                    OsString::from(prompt),
                    OsString::from("--output-format"),
                    OsString::from("stream-json"),
                    OsString::from("--verbose"),
                    OsString::from("--mcp-config"),
                    OsString::from(mcp_config),
                    OsString::from("--strict-mcp-config"),
                    OsString::from("--allowedTools"),
                    OsString::from("Edit"),
                    OsString::from("Write"),
                    OsString::from("mcp__gameengine_editor__*"),
                    OsString::from("--disallowedTools"),
                    OsString::from("Bash"),
                    OsString::from("Task"),
                ],
            })
        }
        ExternalAgentProviderKind::Codex => {
            let mcp_url = format!(
                "mcp_servers.{GAMEENGINE_MCP_SERVER_NAME}.url={}",
                toml_basic_string(mcp_endpoint)
            );
            let bearer_env = format!(
                "mcp_servers.{GAMEENGINE_MCP_SERVER_NAME}.bearer_token_env_var={}",
                toml_basic_string(GAMEENGINE_MCP_TOKEN_ENV)
            );
            let run_header = format!(
                "mcp_servers.{GAMEENGINE_MCP_SERVER_NAME}.env_http_headers.{}={}",
                toml_basic_string(GAMEENGINE_AGENT_RUN_ID_HEADER),
                toml_basic_string(GAMEENGINE_AGENT_RUN_ID_ENV)
            );
            let windows_sandbox = (placement.environment
                == ExternalAgentExecutionEnvironment::WindowsNative)
                .then(|| OsString::from("windows.sandbox=\"elevated\""));
            let mut args = vec![
                OsString::from("exec"),
                OsString::from("--json"),
                OsString::from("--skip-git-repo-check"),
                // Codex otherwise loads the user's global config, which may
                // contain unrelated MCP servers such as a Unity integration.
                // The GameEngine run must expose only the server injected
                // below; authentication remains available because Codex's
                // --ignore-user-config option does not disable CODEX_HOME
                // credential lookup.
                OsString::from("--ignore-user-config"),
                OsString::from("--enable"),
                OsString::from(CODEX_MCP_FEATURE),
                OsString::from("--sandbox"),
                OsString::from("workspace-write"),
                OsString::from("-c"),
                OsString::from(mcp_url),
                OsString::from("-c"),
                OsString::from(bearer_env),
                OsString::from("-c"),
                OsString::from(run_header),
            ];
            if let Some(windows_sandbox) = windows_sandbox {
                args.push(OsString::from("-c"));
                args.push(windows_sandbox);
            }
            args.push(OsString::from(prompt));
            Ok(ExternalAgentLaunchPlan {
                program: OsString::from("codex"),
                args,
            })
        }
        ExternalAgentProviderKind::Generic => {
            if generic_program.trim().is_empty() {
                return Err("Configure a generic external agent command before Go.".to_owned());
            }
            Ok(ExternalAgentLaunchPlan {
                program: OsString::from(generic_program.trim()),
                args: generic_args
                    .iter()
                    .map(|argument| OsString::from(argument.as_str()))
                    .collect(),
            })
        }
    }
}

fn toml_basic_string(value: &str) -> String {
    let mut quoted = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character if character.is_control() => {
                quoted.push_str(&format!("\\u{:04x}", u32::from(character)));
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExternalAgentSemanticEvent {
    Progress {
        step: &'static str,
        detail: &'static str,
    },
    ToolAction {
        tool: String,
        action: &'static str,
        success: Option<bool>,
    },
    GameEngineProtocolPayload(String),
    ProtocolDiagnostic(String),
}

pub(crate) fn translate_provider_line(
    kind: ExternalAgentProviderKind,
    line: &str,
) -> Vec<ExternalAgentSemanticEvent> {
    match kind {
        ExternalAgentProviderKind::ClaudeCode => translate_claude_line(line),
        ExternalAgentProviderKind::Codex => translate_codex_line(line),
        ExternalAgentProviderKind::Generic => Vec::new(),
    }
}

fn translate_claude_line(line: &str) -> Vec<ExternalAgentSemanticEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return vec![ExternalAgentSemanticEvent::ProtocolDiagnostic(format!(
            "Claude Code emitted invalid stream-json: {}",
            truncate_captured_line(line.to_owned())
        ))];
    };
    let mut events = Vec::new();
    match value.get("type").and_then(Value::as_str) {
        Some("system") if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            events.push(ExternalAgentSemanticEvent::Progress {
                step: "provider_connected",
                detail: "Claude Code initialized its external agent session.",
            });
        }
        Some("assistant") => {
            if let Some(content) = value
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
            {
                for item in content {
                    match item.get("type").and_then(Value::as_str) {
                        Some("tool_use") => {
                            let tool = item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("provider_tool")
                                .to_owned();
                            events.push(ExternalAgentSemanticEvent::ToolAction {
                                tool,
                                action: "provider tool requested",
                                success: None,
                            });
                        }
                        Some("text") => {
                            if let Some(text) = item.get("text").and_then(Value::as_str) {
                                collect_protocol_payloads(text, &mut events);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("result") => {
            events.push(ExternalAgentSemanticEvent::Progress {
                step: "provider_turn_finished",
                detail: "Claude Code returned control to the GameEngine host.",
            });
        }
        other => events.push(ExternalAgentSemanticEvent::ProtocolDiagnostic(format!(
            "Claude Code emitted an unsupported event type {other:?}."
        ))),
    }
    events
}

fn translate_codex_line(line: &str) -> Vec<ExternalAgentSemanticEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return vec![ExternalAgentSemanticEvent::ProtocolDiagnostic(format!(
            "Codex emitted invalid --json output: {}",
            truncate_captured_line(line.to_owned())
        ))];
    };
    let mut events = Vec::new();
    match value.get("type").and_then(Value::as_str) {
        Some("thread.started") => events.push(ExternalAgentSemanticEvent::Progress {
            step: "provider_connected",
            detail: "Codex initialized its external agent thread.",
        }),
        Some("turn.started") => events.push(ExternalAgentSemanticEvent::Progress {
            step: "provider_turn_started",
            detail: "Codex started a provider turn.",
        }),
        Some("turn.completed") => events.push(ExternalAgentSemanticEvent::Progress {
            step: "provider_turn_finished",
            detail: "Codex returned control to the GameEngine host.",
        }),
        Some("item.started") | Some("item.completed") => {
            if let Some(item) = value.get("item") {
                match item.get("type").and_then(Value::as_str) {
                    Some("mcp_tool_call") => {
                        let tool = item
                            .get("tool")
                            .or_else(|| item.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("mcp_tool")
                            .to_owned();
                        events.push(ExternalAgentSemanticEvent::ToolAction {
                            tool,
                            action: "provider MCP tool activity",
                            success: None,
                        });
                    }
                    Some("command_execution") => {
                        events.push(ExternalAgentSemanticEvent::ToolAction {
                            tool: "provider.command".to_owned(),
                            action: "provider command activity",
                            success: None,
                        })
                    }
                    Some("file_change") => events.push(ExternalAgentSemanticEvent::ToolAction {
                        tool: "workspace.file_change".to_owned(),
                        action: "provider workspace edit",
                        success: None,
                    }),
                    Some("agent_message") => {
                        if let Some(text) = item
                            .get("text")
                            .or_else(|| item.get("message"))
                            .and_then(Value::as_str)
                        {
                            collect_protocol_payloads(text, &mut events);
                        }
                    }
                    _ => {}
                }
            }
        }
        other => events.push(ExternalAgentSemanticEvent::ProtocolDiagnostic(format!(
            "Codex emitted an unsupported event type {other:?}."
        ))),
    }
    events
}

fn collect_protocol_payloads(text: &str, events: &mut Vec<ExternalAgentSemanticEvent>) {
    for line in text.lines() {
        if let Some(payload) = line.trim().strip_prefix(GAMEENGINE_AGENT_EVENT_PREFIX) {
            events.push(ExternalAgentSemanticEvent::GameEngineProtocolPayload(
                payload.to_owned(),
            ));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ExternalAgentDiagnostics {
    authentication: bool,
    rate_limited: bool,
    mcp: bool,
    configuration: bool,
}

impl ExternalAgentDiagnostics {
    pub(crate) fn observe(&mut self, kind: ExternalAgentProviderKind, line: &str) {
        if kind == ExternalAgentProviderKind::Generic {
            return;
        }
        let lower = line.to_ascii_lowercase();
        self.authentication |= lower.contains("not logged in")
            || lower.contains("authentication failed")
            || lower.contains("unauthorized")
            || lower.contains("sign in required");
        self.rate_limited |= lower.contains("rate limit")
            || lower.contains("rate_limit")
            || lower.contains("\"status\":429")
            || lower.contains("status 429");
        self.mcp |= lower.contains("mcp")
            && (lower.contains("failed")
                || lower.contains("error")
                || lower.contains("unavailable"));
        self.configuration |= lower.contains("configuration error")
            || lower.contains("invalid config")
            || lower.contains("invalid configuration");
    }

    pub(crate) fn classify_exit(
        self,
        kind: ExternalAgentProviderKind,
        exit_code: Option<i32>,
    ) -> ExternalAgentFailureClassification {
        let provider = kind.label();
        if self.authentication {
            return ExternalAgentFailureClassification {
                message: format!("{provider} authentication is unavailable or expired."),
                retryable: false,
            };
        }
        if self.rate_limited {
            return ExternalAgentFailureClassification {
                message: format!("{provider} reported provider-side rate limiting."),
                retryable: true,
            };
        }
        if self.mcp {
            return ExternalAgentFailureClassification {
                message: format!(
                    "{provider} could not use the injected GameEngine MCP connection."
                ),
                retryable: true,
            };
        }
        if self.configuration {
            return ExternalAgentFailureClassification {
                message: format!("{provider} rejected its provider configuration."),
                retryable: false,
            };
        }
        ExternalAgentFailureClassification {
            message: format!("{provider} exited unsuccessfully with {exit_code:?}."),
            retryable: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalAgentFailureClassification {
    pub(crate) message: String,
    pub(crate) retryable: bool,
}

/// Role of one recorded turn handed to a provider-served answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalAgentQuestionRole {
    User,
    Assistant,
    System,
}

impl ExternalAgentQuestionRole {
    /// Returns the label this role carries inside a rendered transcript.
    const fn transcript_label(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Assistant => "Assistant",
            Self::System => "System",
        }
    }
}

/// One recorded conversation turn handed to a provider-served answer.
#[derive(Debug, Clone)]
pub(crate) struct ExternalAgentQuestionTurn {
    pub(crate) role: ExternalAgentQuestionRole,
    pub(crate) text: String,
}

/// Instruction that keeps a provider-served answer inside Ask semantics.
///
/// The read-only launch arguments are the enforcement; this preamble exists so
/// the provider reports an answer instead of proposing edits it cannot apply.
const QUESTION_PREAMBLE: &str = "You are answering inside the GameEngine editor's AI Studio in Ask mode. \
Read whatever project evidence you need, then answer the last user turn. \
Do not create, modify, or delete files, do not run state-changing commands, and do not start a build. \
Reply with the answer only.";

/// Longest provider output line retained for diagnostics.
const MAX_CAPTURED_LINE: usize = 4000;

/// Number of recent provider output lines retained for a failure message.
const MAX_RETAINED_DIAGNOSTIC_LINES: usize = 8;

/// Renders a recorded conversation as the single prompt a provider receives.
pub(crate) fn build_question_prompt(turns: &[ExternalAgentQuestionTurn]) -> String {
    let mut prompt = String::from(QUESTION_PREAMBLE);
    prompt.push_str("\n\nConversation so far:\n");
    for turn in turns {
        let text = turn.text.trim();
        if text.is_empty() {
            continue;
        }
        prompt.push('\n');
        prompt.push_str(turn.role.transcript_label());
        prompt.push_str(": ");
        prompt.push_str(text);
        prompt.push('\n');
    }
    prompt
}

/// Builds the read-only launch plan that answers one Ask turn.
///
/// The plan carries a read-only Editor MCP connection. The credential itself
/// rejects mutating tools, so Ask can inspect unsaved authoritative Editor
/// state without acquiring authoring authority.
///
/// # Errors
///
/// Returns an error when the provider has no launch shape GameEngine can prove
/// is read-only.
pub(crate) fn build_question_launch_plan(
    kind: ExternalAgentProviderKind,
    placement: &ExternalAgentExecutionPlacement,
    prompt: &str,
    mcp_endpoint: &str,
) -> Result<ExternalAgentLaunchPlan, String> {
    let plan = match kind {
        ExternalAgentProviderKind::ClaudeCode => {
            let mcp_config = serde_json::json!({
                "mcpServers": {
                    GAMEENGINE_MCP_SERVER_NAME: {
                        "type": "http",
                        "url": mcp_endpoint,
                        "headers": {
                            "Authorization": format!("Bearer ${{{GAMEENGINE_MCP_TOKEN_ENV}}}"),
                        },
                    }
                }
            })
            .to_string();
            ExternalAgentLaunchPlan {
                program: OsString::from("claude"),
                args: vec![
                    OsString::from("-p"),
                    OsString::from(prompt),
                    OsString::from("--output-format"),
                    OsString::from("stream-json"),
                    OsString::from("--verbose"),
                    OsString::from("--mcp-config"),
                    OsString::from(mcp_config),
                    OsString::from("--strict-mcp-config"),
                    OsString::from("--allowedTools"),
                    OsString::from("Read"),
                    OsString::from("Glob"),
                    OsString::from("Grep"),
                    OsString::from("mcp__gameengine_editor__*"),
                    // An allow list only decides what may run without a prompt, and
                    // a project's own provider settings can widen it. A deny list is
                    // the rule the provider cannot be configured around, so the
                    // write-capable tools are named here explicitly.
                    OsString::from("--disallowedTools"),
                    OsString::from("Write"),
                    OsString::from("Edit"),
                    OsString::from("NotebookEdit"),
                    OsString::from("Bash"),
                    OsString::from("Task"),
                ],
            }
        }
        ExternalAgentProviderKind::Codex => {
            let mcp_url = format!(
                "mcp_servers.{GAMEENGINE_MCP_SERVER_NAME}.url={}",
                toml_basic_string(mcp_endpoint)
            );
            let bearer_env = format!(
                "mcp_servers.{GAMEENGINE_MCP_SERVER_NAME}.bearer_token_env_var={}",
                toml_basic_string(GAMEENGINE_MCP_TOKEN_ENV)
            );
            ExternalAgentLaunchPlan {
                program: OsString::from("codex"),
                args: vec![
                    OsString::from("exec"),
                    OsString::from("--json"),
                    OsString::from("--skip-git-repo-check"),
                    // Ask must use the same MCP isolation as Build. Without
                    // this flag, a read-only question could still discover
                    // and select an unrelated user-configured MCP server.
                    OsString::from("--ignore-user-config"),
                    OsString::from("--enable"),
                    OsString::from(CODEX_MCP_FEATURE),
                    OsString::from("--sandbox"),
                    OsString::from("read-only"),
                    OsString::from("-c"),
                    OsString::from(mcp_url),
                    OsString::from("-c"),
                    OsString::from(bearer_env),
                    OsString::from(prompt),
                ],
            }
        }
        ExternalAgentProviderKind::Generic => {
            return Err(
                "The Generic command provider cannot answer Ask, because GameEngine cannot prove a user-defined command stays read-only. Select Claude Code or Codex, or answer with the selected ModelBackend."
                    .to_owned(),
            );
        }
    };
    let (program, args) = placed_launch_command(placement, plan.program, plan.args);
    ensure_launcher_carries_arguments(&program, &args)?;
    Ok(ExternalAgentLaunchPlan { program, args })
}

/// Extracts assistant answer text from one provider output line.
///
/// Provider streams carry progress, tool activity, and final text on the same
/// channel. Only text a provider states as assistant output is returned, so a
/// diagnostic line never becomes an answer.
fn extract_answer_text(kind: ExternalAgentProviderKind, line: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    match kind {
        ExternalAgentProviderKind::ClaudeCode => match value.get("type").and_then(Value::as_str) {
            Some("result") => {
                if value.get("is_error").and_then(Value::as_bool) == Some(true) {
                    return None;
                }
                non_empty(value.get("result").and_then(Value::as_str)?)
            }
            Some("assistant") => {
                let content = value
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(Value::as_array)?;
                let text = content
                    .iter()
                    .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                non_empty(&text)
            }
            _ => None,
        },
        ExternalAgentProviderKind::Codex => {
            if value.get("type").and_then(Value::as_str) != Some("item.completed") {
                return None;
            }
            let item = value.get("item")?;
            if item.get("type").and_then(Value::as_str) != Some("agent_message") {
                return None;
            }
            let text = item
                .get("text")
                .or_else(|| item.get("message"))
                .and_then(Value::as_str)?;
            non_empty(text)
        }
        ExternalAgentProviderKind::Generic => None,
    }
}

/// Extracts a failure a provider stated on its own stream.
///
/// A provider can report a failed turn and still exit successfully, and its
/// message explains the failure better than an exit code does.
fn extract_error_text(kind: ExternalAgentProviderKind, line: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    match kind {
        ExternalAgentProviderKind::ClaudeCode => {
            if value.get("type").and_then(Value::as_str) != Some("result") {
                return None;
            }
            if value.get("is_error").and_then(Value::as_bool) != Some(true) {
                return None;
            }
            non_empty(value.get("result").and_then(Value::as_str)?)
        }
        ExternalAgentProviderKind::Codex => match value.get("type").and_then(Value::as_str) {
            Some("error") => non_empty(value.get("message").and_then(Value::as_str)?),
            Some("turn.failed") => non_empty(
                value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)?,
            ),
            _ => None,
        },
        ExternalAgentProviderKind::Generic => None,
    }
}

/// Returns the trimmed text unless it is empty.
fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Shortens one captured provider line so diagnostics stay bounded.
fn truncate_captured_line(mut line: String) -> String {
    if line.len() > MAX_CAPTURED_LINE {
        line.truncate(MAX_CAPTURED_LINE);
        line.push('…');
    }
    line
}

/// An answer a provider produced for one Ask turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalAgentAnswer {
    pub(crate) provider: ExternalAgentProviderKind,
    pub(crate) text: String,
    pub(crate) elapsed_ms: u128,
}

/// A running provider process that answers one Ask turn and then exits.
///
/// This is deliberately not an Agent Host run: an answer acquires no work
/// claim and prepares no code workspace. Its separate read-only Editor MCP
/// credential cannot invoke mutation tools, so a question never enters the run
/// lifecycle while still seeing unsaved authoritative state.
pub(crate) struct ExternalAgentQuestionTask {
    kind: ExternalAgentProviderKind,
    result: Receiver<Result<ExternalAgentAnswer, String>>,
    child: Arc<Mutex<Option<Child>>>,
}

impl ExternalAgentQuestionTask {
    /// Starts the provider process that answers one Ask turn.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker thread cannot be created.
    pub(crate) fn spawn(
        kind: ExternalAgentProviderKind,
        plan: ExternalAgentLaunchPlan,
        working_directory: PathBuf,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<Self, String> {
        let (sender, result) = mpsc::channel();
        let child = Arc::new(Mutex::new(None));
        let worker_child = Arc::clone(&child);
        std::thread::Builder::new()
            .name("ai-external-question".to_owned())
            .spawn(move || {
                let answer = answer_with_provider(
                    kind,
                    &plan,
                    &working_directory,
                    &environment,
                    &worker_child,
                );
                let _ = sender.send(answer);
            })
            .map_err(|error| format!("Could not start the provider answer worker: {error}"))?;
        Ok(Self {
            kind,
            result,
            child,
        })
    }

    /// Which provider is answering.
    pub(crate) const fn kind(&self) -> ExternalAgentProviderKind {
        self.kind
    }

    /// Returns the answer once the provider process has finished.
    pub(crate) fn poll(&self) -> Option<Result<ExternalAgentAnswer, String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "The provider answer worker stopped unexpectedly.".to_owned(),
            )),
        }
    }

    /// Terminates the provider process without recording an answer.
    pub(crate) fn cancel(&self) {
        if let Ok(mut guard) = self.child.lock()
            && let Some(child) = guard.as_mut()
        {
            let _ = terminate_process_tree(child);
        }
    }
}

/// Runs one provider answer process to completion on the worker thread.
fn answer_with_provider(
    kind: ExternalAgentProviderKind,
    plan: &ExternalAgentLaunchPlan,
    working_directory: &Path,
    environment: &[(OsString, OsString)],
    child_slot: &Arc<Mutex<Option<Child>>>,
) -> Result<ExternalAgentAnswer, String> {
    let started = Instant::now();
    let mut child = Command::new(&plan.program)
        .args(&plan.args)
        .current_dir(working_directory)
        .envs(environment.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not start {}: {error}", kind.label()))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (line_sender, lines) = mpsc::channel::<(ProcessStreamKind, String)>();
    for pipe in [
        stdout.map(PipeReader::Stdout),
        stderr.map(PipeReader::Stderr),
    ]
    .into_iter()
    .flatten()
    {
        let sender = line_sender.clone();
        let stream = pipe.stream_kind();
        if let Err(error) = std::thread::Builder::new()
            .name("ai-external-question-output".to_owned())
            .spawn(move || {
                for line in pipe.lines() {
                    let _ = sender.send((stream, line));
                }
            })
        {
            let _ = terminate_process_tree(&mut child);
            return Err(format!("Could not read {} output: {error}", kind.label()));
        }
    }
    drop(line_sender);
    let Ok(mut guard) = child_slot.lock() else {
        // The slot exists only for cancellation, but a poisoned lock must not
        // leave a provider process running with nothing able to stop it.
        let _ = terminate_process_tree(&mut child);
        return Err(format!(
            "Could not track the {} process for cancellation.",
            kind.label()
        ));
    };
    *guard = Some(child);
    drop(guard);
    let mut answer: Option<String> = None;
    let mut diagnostics = ExternalAgentDiagnostics::default();
    let mut recent = Vec::new();
    let mut stated_error: Option<String> = None;
    for (stream, line) in lines {
        diagnostics.observe(kind, &line);
        if stream == ProcessStreamKind::Stdout {
            if let Some(text) = extract_answer_text(kind, &line) {
                answer = Some(text);
            }
            if let Some(text) = extract_error_text(kind, &line) {
                stated_error = Some(text);
            }
        }
        recent.push(truncate_captured_line(line));
        if recent.len() > MAX_RETAINED_DIAGNOSTIC_LINES {
            recent.remove(0);
        }
    }
    let status = {
        let Ok(mut guard) = child_slot.lock() else {
            return Err(format!(
                "Could not wait for the {} process to exit.",
                kind.label()
            ));
        };
        let Some(child) = guard.as_mut() else {
            return Err(format!(
                "The {} process is no longer tracked.",
                kind.label()
            ));
        };
        child
            .wait()
            .map_err(|error| format!("Could not wait for {}: {error}", kind.label()))?
    };
    if let Ok(mut guard) = child_slot.lock() {
        *guard = None;
    }
    // A provider can report a failed turn and still exit successfully, so the
    // failure it stated decides the outcome before the exit code does.
    if !status.success() || stated_error.is_some() {
        return Err(question_failure_message(
            kind,
            diagnostics,
            status.code(),
            stated_error.as_deref(),
            &recent,
        ));
    }
    let Some(text) = answer else {
        return Err(format!(
            "{} finished without returning an answer. Recent provider output: {}",
            kind.label(),
            join_recent(&recent)
        ));
    };
    Ok(ExternalAgentAnswer {
        provider: kind,
        text,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// Which pipe one captured provider line arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessStreamKind {
    Stdout,
    Stderr,
}

/// Builds the message an unsuccessful provider answer reports.
///
/// A failure the provider stated is reported as the provider stated it. The
/// classified exit remains the fallback for a process that failed without
/// explaining itself.
fn question_failure_message(
    kind: ExternalAgentProviderKind,
    diagnostics: ExternalAgentDiagnostics,
    exit_code: Option<i32>,
    stated_error: Option<&str>,
    recent: &[String],
) -> String {
    if let Some(stated) = stated_error {
        return format!("{} reported: {stated}", kind.label());
    }
    let classification = diagnostics.classify_exit(kind, exit_code);
    format!(
        "{} Recent provider output: {}",
        classification.message,
        join_recent(recent)
    )
}

/// Renders retained provider output for a diagnostic message.
fn join_recent(recent: &[String]) -> String {
    if recent.is_empty() {
        return "none".to_owned();
    }
    recent.join(" | ")
}

/// A setup step the Editor can run for a provider on the user's behalf.
///
/// Both steps are provider-owned work: the provider's own installer package and
/// the provider's own login flow. GameEngine starts them so the setup path does
/// not leave the Editor, and owns neither the artifact nor the credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalAgentSetupAction {
    Install,
    SignIn,
}

impl ExternalAgentSetupAction {
    /// Returns the label a control for this step carries.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Install => "Install or update",
            Self::SignIn => "Sign in",
        }
    }

    /// Returns what this step is doing, for a progress line.
    pub(crate) const fn progress_label(self) -> &'static str {
        match self {
            Self::Install => "Installing or updating the provider",
            Self::SignIn => "Provider sign-in is running",
        }
    }

    /// Builds the command this step runs for one provider.
    ///
    /// # Errors
    ///
    /// Returns an error for a provider GameEngine cannot perform this step for.
    fn plan(
        self,
        kind: ExternalAgentProviderKind,
        placement: &ExternalAgentExecutionPlacement,
    ) -> Result<ExternalAgentLaunchPlan, String> {
        match self {
            Self::Install => build_install_plan(kind, placement),
            Self::SignIn => build_sign_in_plan(kind, placement),
        }
    }
}

/// The npm package that publishes each first-class provider CLI.
const fn install_package(kind: ExternalAgentProviderKind) -> Option<&'static str> {
    match kind {
        ExternalAgentProviderKind::ClaudeCode => Some("@anthropic-ai/claude-code@2.1.237"),
        ExternalAgentProviderKind::Codex => Some("@openai/codex@0.148.0"),
        ExternalAgentProviderKind::Generic => None,
    }
}

/// Returns the npm executable name for one execution environment.
///
/// On Windows the launcher is `npm.cmd`; process creation appends only `.exe`
/// to a bare name, so the extension has to be explicit or the launch fails with
/// a misleading "program not found".
const fn npm_program(placement: &ExternalAgentExecutionPlacement) -> &'static str {
    match placement.environment {
        ExternalAgentExecutionEnvironment::WindowsNative => "npm.cmd",
        ExternalAgentExecutionEnvironment::Wsl2Linux => "npm",
    }
}

/// Builds the command that installs or updates a provider CLI.
///
/// The version is pinned to the adapter version validated by GameEngine.
/// Updating a provider therefore requires updating and testing its stream
/// parser, command-line flags, MCP behavior, and credential probe together.
///
/// # Errors
///
/// Returns an error for a provider GameEngine does not publish an install
/// command for.
pub(crate) fn build_install_plan(
    kind: ExternalAgentProviderKind,
    placement: &ExternalAgentExecutionPlacement,
) -> Result<ExternalAgentLaunchPlan, String> {
    let Some(package) = install_package(kind) else {
        return Err(
            "A generic external command is installed by whoever provides it. GameEngine cannot install it."
                .to_owned(),
        );
    };
    let (program, args) = placed_command(
        placement,
        OsString::from(npm_program(placement)),
        [
            OsString::from("install"),
            OsString::from("-g"),
            OsString::from(package),
        ],
    );
    Ok(ExternalAgentLaunchPlan { program, args })
}

/// Renders the command a setup step will run, for display before it runs.
///
/// The user is agreeing to run a specific command on their machine, so the
/// command is shown rather than described.
pub(crate) fn setup_command_text(
    action: ExternalAgentSetupAction,
    kind: ExternalAgentProviderKind,
    placement: &ExternalAgentExecutionPlacement,
) -> Option<String> {
    let plan = action.plan(kind, placement).ok()?;
    let mut text = plan.program.to_string_lossy().into_owned();
    for argument in &plan.args {
        text.push(' ');
        text.push_str(&argument.to_string_lossy());
    }
    Some(text)
}

/// Builds the command that starts a provider's own sign-in flow.
///
/// # Errors
///
/// Returns an error for a provider whose authentication GameEngine does not
/// own and cannot start.
pub(crate) fn build_sign_in_plan(
    kind: ExternalAgentProviderKind,
    placement: &ExternalAgentExecutionPlacement,
) -> Result<ExternalAgentLaunchPlan, String> {
    let plan = match kind {
        ExternalAgentProviderKind::ClaudeCode => ExternalAgentLaunchPlan {
            program: OsString::from("claude"),
            args: vec![
                OsString::from("auth"),
                OsString::from("login"),
                // The subscription flow is the one AI Studio advertises; an
                // API-billed console login stays a provider-side choice.
                OsString::from("--claudeai"),
            ],
        },
        ExternalAgentProviderKind::Codex => ExternalAgentLaunchPlan {
            program: OsString::from("codex"),
            args: vec![OsString::from("login")],
        },
        ExternalAgentProviderKind::Generic => {
            return Err(
                "A generic external command owns its own authentication. GameEngine cannot start a sign-in for it."
                    .to_owned(),
            );
        }
    };
    let (program, args) = placed_command(placement, plan.program, plan.args);
    Ok(ExternalAgentLaunchPlan { program, args })
}

/// A provider setup step the Editor started on the user's behalf.
///
/// GameEngine owns neither side of what these steps produce: an install is the
/// provider's own published package, and a sign-in stores the credential where
/// the provider keeps it. The Editor starts the process, relays its output, and
/// allows cancellation, so setup does not require opening a terminal.
pub(crate) struct ExternalAgentSetupTask {
    kind: ExternalAgentProviderKind,
    action: ExternalAgentSetupAction,
    child: Child,
    input: Option<std::process::ChildStdin>,
    output: Receiver<String>,
    exit_status: Option<ExitStatus>,
}

impl ExternalAgentSetupTask {
    /// Starts one provider setup step.
    ///
    /// # Errors
    ///
    /// Returns an error when the step does not apply to this provider, the
    /// command cannot be launched, or its output readers cannot be created.
    pub(crate) fn spawn(
        kind: ExternalAgentProviderKind,
        action: ExternalAgentSetupAction,
        placement: &ExternalAgentExecutionPlacement,
        working_directory: &Path,
    ) -> Result<Self, String> {
        let plan = action.plan(kind, placement)?;
        // The rendered command names the provider, because that is what the
        // user agreed to run. Process creation needs the launcher file that
        // name resolves to on this machine, which is resolved here so the
        // displayed command stays the command and not a machine path.
        let (program, args) = resolve_launcher(placement, plan.program).with_arguments(plan.args);
        let mut child = Command::new(&program)
            .args(&args)
            .current_dir(working_directory)
            .stdin(if action == ExternalAgentSetupAction::SignIn {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                format!(
                    "Could not start {} for {}: {error}",
                    program.to_string_lossy(),
                    kind.label()
                )
            })?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let input = child.stdin.take();
        let (sender, output) = mpsc::channel();
        for pipe in [
            stdout.map(PipeReader::Stdout),
            stderr.map(PipeReader::Stderr),
        ]
        .into_iter()
        .flatten()
        {
            let sender = sender.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("ai-external-provider-setup".to_owned())
                .spawn(move || {
                    for line in pipe.lines() {
                        let _ = sender.send(line);
                    }
                })
            {
                let _ = terminate_process_tree(&mut child);
                return Err(format!(
                    "Could not read {} setup output: {error}",
                    kind.label()
                ));
            }
        }
        Ok(Self {
            kind,
            action,
            child,
            input,
            output,
            exit_status: None,
        })
    }

    /// Which provider this step is setting up.
    pub(crate) const fn kind(&self) -> ExternalAgentProviderKind {
        self.kind
    }

    /// Which step is running.
    pub(crate) const fn action(&self) -> ExternalAgentSetupAction {
        self.action
    }

    /// Returns provider output produced since the previous call.
    pub(crate) fn drain_output(&self) -> Vec<String> {
        self.output.try_iter().collect()
    }

    /// Sends one confirmation or device-code response to an interactive
    /// provider sign-in flow.
    pub(crate) fn send_input(&mut self, input: &str) -> Result<(), String> {
        use std::io::Write;

        let Some(stdin) = self.input.as_mut() else {
            return Err("This provider setup step is not accepting input.".to_owned());
        };
        stdin
            .write_all(input.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("Could not send provider sign-in input: {error}"))
    }

    /// Returns the exit status once the setup process has finished.
    ///
    /// # Errors
    ///
    /// Returns an error when the process state cannot be read.
    pub(crate) fn poll_exit(&mut self) -> Result<Option<ExitStatus>, String> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        let status = self.child.try_wait().map_err(|error| {
            format!("Could not read {} setup state: {error}", self.kind.label())
        })?;
        self.exit_status = status;
        Ok(status)
    }

    /// Stops an unfinished setup step.
    pub(crate) fn cancel(&mut self) {
        if self.exit_status.is_none() {
            self.exit_status = terminate_process_tree(&mut self.child).ok().flatten();
        }
    }
}

impl Drop for ExternalAgentSetupTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// One captured provider pipe, kept concrete so both readers share a body.
enum PipeReader {
    Stdout(std::process::ChildStdout),
    Stderr(std::process::ChildStderr),
}

impl PipeReader {
    /// Which stream this pipe carries.
    const fn stream_kind(&self) -> ProcessStreamKind {
        match self {
            Self::Stdout(_) => ProcessStreamKind::Stdout,
            Self::Stderr(_) => ProcessStreamKind::Stderr,
        }
    }

    /// Reads the pipe to end of stream without modifying provider JSON.
    fn lines(self) -> Box<dyn Iterator<Item = String>> {
        match self {
            Self::Stdout(pipe) => Box::new(BufReader::new(pipe).lines().map_while(Result::ok)),
            Self::Stderr(pipe) => Box::new(BufReader::new(pipe).lines().map_while(Result::ok)),
        }
    }
}

/// Extracts the sign-in URL a provider printed, if the line carries one.
///
/// A provider that cannot open a browser itself prints the URL instead. The
/// Editor surfaces it so the flow can be completed without a terminal.
pub(crate) fn sign_in_url(line: &str) -> Option<String> {
    let start = line.find("https://").or_else(|| line.find("http://"))?;
    let url = line[start..]
        .split_whitespace()
        .next()?
        .trim_end_matches(['.', ',', ')', ']', '"', '\'']);
    (url.len() > "https://".len()).then(|| url.to_owned())
}

/// What one probe learned about a provider on this machine.
///
/// Locations are machine-local paths kept for local display only. They are not
/// part of the sanitized adapter status ADR 0145 reports remotely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalAgentProviderReport {
    pub(crate) status: ExternalAgentProviderStatus,
    pub(crate) locations: Vec<String>,
    /// Whether the installer this provider is installed with is available.
    pub(crate) installer_available: bool,
}

impl ExternalAgentProviderReport {
    /// Whether more than one directory on `PATH` provides this program.
    ///
    /// An install can land beside an older copy that still comes first on
    /// `PATH`, which otherwise looks like an update that did nothing.
    pub(crate) fn has_shadowed_copies(&self) -> bool {
        let mut directories = self
            .locations
            .iter()
            .filter_map(|location| {
                let path = Path::new(location);
                path.parent()
                    .map(|parent| parent.to_string_lossy().into_owned())
            })
            .collect::<Vec<_>>();
        directories.sort();
        directories.dedup();
        directories.len() > 1
    }
}

/// Resolves every `PATH` entry that provides one provider program.
fn resolve_program_locations(
    kind: ExternalAgentProviderKind,
    placement: &ExternalAgentExecutionPlacement,
) -> Vec<String> {
    let Some(program) = kind.program() else {
        return Vec::new();
    };
    let (locator, locator_args) = match placement.environment {
        ExternalAgentExecutionEnvironment::WindowsNative => {
            (OsString::from("where.exe"), vec![program.to_os_string()])
        }
        ExternalAgentExecutionEnvironment::Wsl2Linux => {
            let (program, args) = placed_command(
                placement,
                OsString::from("bash"),
                [
                    OsString::from("-lc"),
                    OsString::from(format!("command -v {}", program.to_string_lossy())),
                ],
            );
            (program, args)
        }
    };
    let Ok((succeeded, output)) = direct_command_output(locator, locator_args) else {
        return Vec::new();
    };
    if !succeeded {
        return Vec::new();
    }
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(truncate_captured_line_str)
        .collect()
}

/// Shortens one resolved path so a display line stays bounded.
fn truncate_captured_line_str(line: &str) -> String {
    truncate_captured_line(line.to_owned())
}

/// Whether the installer used for provider CLIs can be launched here.
fn installer_is_available(placement: &ExternalAgentExecutionPlacement) -> bool {
    let (program, args) = placed_command(
        placement,
        OsString::from(npm_program(placement)),
        [OsString::from("--version")],
    );
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    wait_for_probe(&mut child)
        .map(|status| status.success())
        .unwrap_or(false)
}

/// A background probe of every first-class provider on this machine.
///
/// Probing runs provider processes, so it never runs on the UI thread.
pub(crate) struct ExternalAgentProbeTask {
    result: Receiver<Vec<ExternalAgentProviderReport>>,
}

impl ExternalAgentProbeTask {
    /// Starts probing discovery, authentication, and placement of providers.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker thread cannot be created.
    pub(crate) fn spawn(placement: ExternalAgentExecutionPlacement) -> Result<Self, String> {
        let (sender, result) = mpsc::channel();
        std::thread::Builder::new()
            .name("ai-external-provider-probe".to_owned())
            .spawn(move || {
                let installer_available = installer_is_available(&placement);
                let reports = ExternalAgentProviderKind::ALL
                    .into_iter()
                    .filter(|kind| kind.can_answer_questions())
                    .map(|kind| ExternalAgentProviderReport {
                        status: probe_provider(kind, "", &placement),
                        locations: resolve_program_locations(kind, &placement),
                        installer_available,
                    })
                    .collect::<Vec<_>>();
                let _ = sender.send(reports);
            })
            .map_err(|error| format!("Could not start the provider probe: {error}"))?;
        Ok(Self { result })
    }

    /// Returns the probe reports once the worker has finished.
    pub(crate) fn poll(&self) -> Option<Vec<ExternalAgentProviderReport>> {
        match self.result.try_recv() {
            Ok(reports) => Some(reports),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wsl_placement_runs_the_same_provider_arguments_through_the_distribution() {
        let placement = ExternalAgentExecutionPlacement {
            environment: ExternalAgentExecutionEnvironment::Wsl2Linux,
            distribution: "Ubuntu-24.04".to_owned(),
        };
        let native = build_launch_plan(
            ExternalAgentProviderKind::ClaudeCode,
            &ExternalAgentExecutionPlacement::windows_native(),
            "",
            &[],
            "task",
            "http://127.0.0.1:1234/mcp",
        )
        .expect("native plan");
        let wsl = build_launch_plan(
            ExternalAgentProviderKind::ClaudeCode,
            &placement,
            "",
            &[],
            "task",
            "http://127.0.0.1:1234/mcp",
        )
        .expect("wsl plan");
        assert_eq!(wsl.program, OsString::from("wsl.exe"));
        // The distribution resolves the provider name itself, so the Linux
        // program crosses bare while a Windows-native launch resolves the
        // launcher file process creation on this machine accepts.
        assert_eq!(
            wsl.args[..4],
            [
                OsString::from("-d"),
                OsString::from("Ubuntu-24.04"),
                OsString::from("--"),
                OsString::from("claude"),
            ]
        );
        // The provider argument vector crosses unchanged, so no shell re-quotes
        // the prompt or the injected MCP configuration.
        assert_eq!(&wsl.args[4..], native.args.as_slice());
    }

    #[test]
    fn a_signed_out_claude_status_report_is_not_read_as_a_credential() {
        let report = r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#;

        assert!(!claude_credential_present(report, true));
    }

    #[test]
    fn a_signed_in_claude_status_report_is_read_as_a_credential() {
        let report = r#"{"loggedIn":true,"authMethod":"claudeai"}"#;

        assert!(claude_credential_present(report, true));
    }

    #[test]
    fn an_unreadable_claude_status_report_fails_closed() {
        assert!(!claude_credential_present("not a status report", true));
        assert!(!claude_credential_present("not a status report", false));
    }

    #[test]
    fn an_npm_installed_provider_launches_through_the_extension_windows_requires() {
        let directories = [PathBuf::from("tools"), PathBuf::from("npm")];

        let resolved = resolve_windows_launcher(
            OsStr::new("claude"),
            &directories,
            &|candidate| candidate == Path::new("npm").join("claude.cmd"),
            &|_| None,
        );

        assert_eq!(
            resolved,
            Some(ResolvedLauncher::direct(
                Path::new("npm").join("claude.cmd").into_os_string()
            ))
        );
    }

    #[test]
    fn an_executable_wins_over_a_shim_in_the_same_directory() {
        let directories = [PathBuf::from("tools")];

        let resolved = resolve_windows_launcher(
            OsStr::new("claude"),
            &directories,
            &|candidate| {
                candidate == Path::new("tools").join("claude.exe")
                    || candidate == Path::new("tools").join("claude.cmd")
            },
            &|_| None,
        );

        assert_eq!(
            resolved,
            Some(ResolvedLauncher::direct(
                Path::new("tools").join("claude.exe").into_os_string()
            ))
        );
    }

    #[test]
    fn a_program_that_already_names_its_extension_is_launched_as_written() {
        let directories = [PathBuf::from("tools")];

        let resolved = resolve_windows_launcher(
            OsStr::new("npm.cmd"),
            &directories,
            &|_| unreachable!(),
            &|_| unreachable!(),
        );

        assert_eq!(resolved, None);
    }

    #[test]
    fn an_npm_shim_launches_the_provider_executable_it_forwards_to() {
        let directories = [PathBuf::from("npm")];
        let shim = Path::new("npm").join("claude.cmd");
        let executable = Path::new("npm").join("node_modules").join("claude.exe");

        let resolved = resolve_windows_launcher(
            OsStr::new("claude"),
            &directories,
            &|candidate| candidate == shim || candidate == executable,
            &|candidate| {
                (candidate == shim)
                    .then(|| "@ECHO off\r\n\"%dp0%/node_modules/claude.exe\"   %*\r\n".to_owned())
            },
        );

        // The shim writes its own separator style, so the resolved launcher is
        // compared as a path rather than as raw text.
        let resolved = resolved.expect("the shim names an executable next to itself");
        assert_eq!(PathBuf::from(resolved.program), executable);
        assert!(resolved.leading_args.is_empty());
    }

    /// The shim npm writes for a package whose command is a script.
    ///
    /// The interpreter is chosen at run time, and the script path is the
    /// launcher's own argument, so both have to survive resolution.
    const NPM_SCRIPT_SHIM: &str = concat!(
        "@ECHO off\r\n",
        "GOTO start\r\n",
        ":find_dp0\r\n",
        "SET dp0=%~dp0\r\n",
        "EXIT /b\r\n",
        ":start\r\n",
        "SETLOCAL\r\n",
        "CALL :find_dp0\r\n",
        "\r\n",
        "IF EXIST \"%dp0%/node.exe\" (\r\n",
        "  SET \"_prog=%dp0%/node.exe\"\r\n",
        ") ELSE (\r\n",
        "  SET \"_prog=node\"\r\n",
        "  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n",
        ")\r\n",
        "\r\n",
        "endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  ",
        "\"%dp0%/node_modules/@openai/codex/bin/codex.js\" %*\r\n",
    );

    #[test]
    fn an_npm_script_shim_launches_the_interpreter_it_selects_with_the_provider_script() {
        let directories = [PathBuf::from("npm")];
        let shim = Path::new("npm").join("codex.cmd");
        let interpreter = Path::new("npm").join("node.exe");
        let script = Path::new("npm")
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin")
            .join("codex.js");

        let resolved = resolve_windows_launcher(
            OsStr::new("codex"),
            &directories,
            &|candidate| candidate == shim || candidate == interpreter || candidate == script,
            &|_| Some(NPM_SCRIPT_SHIM.to_owned()),
        )
        .expect("the shim names an interpreter and a script");

        assert_eq!(PathBuf::from(resolved.program), interpreter);
        assert_eq!(
            resolved
                .leading_args
                .into_iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>(),
            vec![script]
        );
    }

    #[test]
    fn a_script_shim_without_a_neighbouring_interpreter_keeps_the_name_on_path() {
        let directories = [PathBuf::from("npm")];
        let shim = Path::new("npm").join("codex.cmd");
        let script = Path::new("npm")
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin")
            .join("codex.js");

        let resolved = resolve_windows_launcher(
            OsStr::new("codex"),
            &directories,
            &|candidate| candidate == shim || candidate == script,
            &|_| Some(NPM_SCRIPT_SHIM.to_owned()),
        )
        .expect("the shim falls back to the interpreter on PATH");

        assert_eq!(resolved.program, OsString::from("node"));
    }

    #[test]
    fn a_shim_naming_a_variable_it_does_not_set_is_launched_as_written() {
        let directories = [PathBuf::from("npm")];
        let shim = Path::new("npm").join("codex.cmd");

        let resolved = resolve_windows_launcher(
            OsStr::new("codex"),
            &directories,
            &|candidate| candidate == shim,
            &|_| Some("\"%CODEX_HOME%/codex.exe\" %*\r\n".to_owned()),
        );

        assert_eq!(
            resolved,
            Some(ResolvedLauncher::direct(shim.into_os_string()))
        );
    }

    #[test]
    fn a_multi_line_prompt_is_refused_before_a_batch_shim_receives_it() {
        let refusal = ensure_launcher_carries_arguments(
            OsStr::new(r"C:\npm\claude.cmd"),
            &[OsString::from("-p"), OsString::from("first\nsecond")],
        )
        .expect_err("a batch shim cannot carry an argument containing a line break");

        assert!(refusal.contains("claude.cmd"), "{refusal}");
    }

    #[test]
    fn an_executable_launcher_carries_a_multi_line_prompt() {
        assert!(
            ensure_launcher_carries_arguments(
                OsStr::new(r"C:\npm\claude.exe"),
                &[OsString::from("-p"), OsString::from("first\nsecond")],
            )
            .is_ok()
        );
    }

    #[test]
    fn a_single_line_batch_launch_stays_allowed() {
        assert!(
            ensure_launcher_carries_arguments(
                OsStr::new("npm.cmd"),
                &[OsString::from("install"), OsString::from("-g")],
            )
            .is_ok()
        );
    }

    #[test]
    fn a_provider_missing_from_every_path_directory_keeps_its_reported_name() {
        let placement = ExternalAgentExecutionPlacement::windows_native();

        let resolved = resolve_launcher(&placement, OsString::from("gameengine-absent-provider"));

        assert_eq!(
            resolved,
            ResolvedLauncher::direct(OsString::from("gameengine-absent-provider"))
        );
    }

    #[test]
    fn a_wsl_placement_keeps_the_bare_linux_program_name() {
        let placement = ExternalAgentExecutionPlacement {
            environment: ExternalAgentExecutionEnvironment::Wsl2Linux,
            distribution: "Ubuntu-24.04".to_owned(),
        };

        let resolved = resolve_launcher(&placement, OsString::from("claude"));

        assert_eq!(resolved, ResolvedLauncher::direct(OsString::from("claude")));
    }

    #[test]
    fn a_default_distribution_placement_omits_the_distribution_selector() {
        let placement = ExternalAgentExecutionPlacement {
            environment: ExternalAgentExecutionEnvironment::Wsl2Linux,
            distribution: "  ".to_owned(),
        };
        let plan = build_launch_plan(
            ExternalAgentProviderKind::Codex,
            &placement,
            "",
            &[],
            "task",
            "http://127.0.0.1:4321/mcp",
        )
        .expect("wsl plan");
        assert_eq!(plan.program, OsString::from("wsl.exe"));
        assert_eq!(plan.args[0], OsString::from("--"));
        assert_eq!(plan.args[1], OsString::from("codex"));
    }

    #[test]
    fn environment_forwarding_names_every_variable_and_marks_paths() {
        let variables = vec![
            (
                OsString::from("GAMEENGINE_MCP_AUTH_TOKEN"),
                OsString::from("token"),
            ),
            (
                OsString::from("GAMEENGINE_AGENT_CAPTURE_PATH"),
                OsString::from("C:\\frames\\frame.png"),
            ),
        ];
        let (name, value) =
            wsl_environment_forwarding(&variables, &["GAMEENGINE_AGENT_CAPTURE_PATH"]);
        assert_eq!(name, OsString::from("WSLENV"));
        let value = value.to_string_lossy();
        assert!(value.contains("GAMEENGINE_MCP_AUTH_TOKEN"));
        assert!(value.contains("GAMEENGINE_AGENT_CAPTURE_PATH/p"));
    }

    #[test]
    fn a_windows_native_placement_never_probes_wsl_reachability() {
        assert!(
            probe_wsl_loopback_reachability(
                &ExternalAgentExecutionPlacement::windows_native(),
                "http://127.0.0.1:1234/mcp",
                "token",
            )
            .is_ok()
        );
    }

    #[test]
    fn a_non_loopback_endpoint_is_rejected_before_a_wsl_launch() {
        let placement = ExternalAgentExecutionPlacement {
            environment: ExternalAgentExecutionEnvironment::Wsl2Linux,
            distribution: String::new(),
        };
        assert!(
            probe_wsl_loopback_reachability(&placement, "https://example.test/mcp", "token")
                .is_err()
        );
    }

    #[test]
    fn editor_mcp_preflight_rejects_non_loopback_endpoints() {
        let error = probe_editor_mcp_endpoint("http://192.0.2.1:4321/mcp", "token")
            .expect_err("non-loopback MCP endpoints must be rejected");

        assert!(error.contains("loopback-only"), "{error}");
    }

    #[test]
    fn editor_mcp_preflight_reports_unreachable_loopback_endpoints() {
        let error = probe_editor_mcp_endpoint("http://127.0.0.1:1/mcp", "token")
            .expect_err("the reserved discard port must not accept MCP");

        assert!(error.contains("Editor MCP endpoint"), "{error}");
    }

    #[test]
    fn generic_launch_plan_preserves_direct_argument_semantics() {
        let args = vec![
            "--flag".to_owned(),
            "value".to_owned(),
            ";".to_owned(),
            "echo".to_owned(),
            "nope".to_owned(),
        ];
        let plan = build_launch_plan(
            ExternalAgentProviderKind::Generic,
            &ExternalAgentExecutionPlacement::windows_native(),
            "custom-agent",
            &args,
            "ignored",
            "http://127.0.0.1:1/mcp",
        )
        .expect("generic plan");
        assert_eq!(plan.program, OsString::from("custom-agent"));
        assert_eq!(
            plan.args,
            vec![
                OsString::from("--flag"),
                OsString::from("value"),
                OsString::from(";"),
                OsString::from("echo"),
                OsString::from("nope"),
            ]
        );
    }

    #[test]
    fn claude_mcp_config_is_valid_json_and_uses_ephemeral_environment() {
        let plan = build_launch_plan(
            ExternalAgentProviderKind::ClaudeCode,
            &ExternalAgentExecutionPlacement::windows_native(),
            "",
            &[],
            "task",
            "http://127.0.0.1:1234/mcp",
        )
        .expect("claude plan");
        let config_index = plan
            .args
            .iter()
            .position(|value| value == OsStr::new("--mcp-config"))
            .expect("mcp config flag");
        let config = plan.args[config_index + 1]
            .to_str()
            .expect("UTF-8 MCP config");
        let parsed: Value = serde_json::from_str(config).expect("valid MCP config JSON");
        let server = &parsed["mcpServers"][GAMEENGINE_MCP_SERVER_NAME];
        assert_eq!(server["url"], "http://127.0.0.1:1234/mcp");
        assert_eq!(
            server["headers"]["Authorization"],
            "Bearer ${GAMEENGINE_MCP_AUTH_TOKEN}"
        );
        assert_eq!(
            server["headers"][GAMEENGINE_AGENT_RUN_ID_HEADER],
            "${GAMEENGINE_AGENT_RUN_ID}"
        );
        let allowed_index = plan
            .args
            .iter()
            .position(|value| value == OsStr::new("--allowedTools"))
            .expect("allowed tools flag");
        assert_eq!(
            &plan.args[allowed_index + 1..allowed_index + 4],
            &[
                OsString::from("Edit"),
                OsString::from("Write"),
                OsString::from("mcp__gameengine_editor__*"),
            ]
        );
    }

    #[test]
    fn codex_mcp_config_uses_bearer_environment_and_workspace_sandbox() {
        let plan = build_launch_plan(
            ExternalAgentProviderKind::Codex,
            &ExternalAgentExecutionPlacement::windows_native(),
            "",
            &[],
            "task",
            "http://127.0.0.1:4321/mcp",
        )
        .expect("codex plan");
        let args = plan
            .args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(args.contains(&format!("--enable\n{CODEX_MCP_FEATURE}")));
        assert!(args.contains("--sandbox\nworkspace-write"));
        assert!(args.contains("http://127.0.0.1:4321/mcp"));
        assert!(args.contains("GAMEENGINE_MCP_AUTH_TOKEN"));
        assert!(args.contains(GAMEENGINE_AGENT_RUN_ID_HEADER));
        assert!(args.contains("windows.sandbox=\"elevated\""));
        assert!(args.contains("--ignore-user-config"));
    }

    #[test]
    fn claude_stream_translation_keeps_host_protocol_explicit() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": r#"GAMEENGINE_AGENT_EVENT {"type":"progress","step":"inspect","detail":"scene"}"#,
                    },
                    {
                        "type": "tool_use",
                        "name": "mcp__gameengine_editor__scene_get",
                    },
                ]
            }
        })
        .to_string();
        let events = translate_provider_line(ExternalAgentProviderKind::ClaudeCode, &line);
        assert!(events.iter().any(|event| matches!(
            event,
            ExternalAgentSemanticEvent::GameEngineProtocolPayload(payload)
                if payload.contains("\"type\":\"progress\"")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ExternalAgentSemanticEvent::ToolAction { tool, .. }
                if tool == "mcp__gameengine_editor__scene_get"
        )));
    }

    #[test]
    fn provider_translation_preserves_an_event_beyond_four_thousand_characters() {
        let long_detail = "x".repeat(8_000);
        let payload = serde_json::json!({
            "type": "progress",
            "step": "inspect",
            "detail": long_detail,
        })
        .to_string();
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "text",
                    "text": format!("{GAMEENGINE_AGENT_EVENT_PREFIX}{payload}"),
                }]
            }
        })
        .to_string();

        let events = translate_provider_line(ExternalAgentProviderKind::ClaudeCode, &line);
        assert!(events.iter().any(|event| matches!(
            event,
            ExternalAgentSemanticEvent::GameEngineProtocolPayload(found) if found == &payload
        )));
    }

    #[test]
    fn invalid_provider_json_becomes_an_explicit_protocol_diagnostic() {
        let events = translate_provider_line(ExternalAgentProviderKind::Codex, "{truncated");
        assert!(matches!(
            events.as_slice(),
            [ExternalAgentSemanticEvent::ProtocolDiagnostic(message)]
                if message.contains("invalid --json output")
        ));
    }

    #[test]
    fn provider_failure_mapping_is_sanitized_and_classified() {
        let mut diagnostics = ExternalAgentDiagnostics::default();
        diagnostics.observe(ExternalAgentProviderKind::Codex, "rate limit exceeded");
        let failure = diagnostics.classify_exit(ExternalAgentProviderKind::Codex, Some(1));
        assert!(failure.retryable);
        assert!(failure.message.contains("rate limiting"));
        assert!(!failure.message.contains("turn.failed"));
    }

    #[test]
    fn remote_status_contains_only_sanitized_adapter_state() {
        let status = ExternalAgentProviderStatus {
            kind: ExternalAgentProviderKind::ClaudeCode,
            discovery: ExternalAgentDiscoveryStatus::Available,
            auth: ExternalAgentAuthStatus::Authenticated,
        };
        let json = status.remote_json().to_string();
        assert!(json.contains("claude-code"));
        assert!(json.contains("authenticated"));
        assert!(!json.contains("GAMEENGINE_MCP"));
        assert!(!json.contains("program"));
    }

    #[test]
    fn a_question_launch_plan_carries_read_only_mcp_and_no_write_surface() {
        let placement = ExternalAgentExecutionPlacement::windows_native();
        for kind in [
            ExternalAgentProviderKind::ClaudeCode,
            ExternalAgentProviderKind::Codex,
        ] {
            let plan = build_question_launch_plan(
                kind,
                &placement,
                "why is the player falling?",
                "http://127.0.0.1:1234/mcp",
            )
            .expect("first-class providers answer questions");
            let args = plan
                .args
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!args.contains("workspace-write"));
            assert!(args.contains(GAMEENGINE_MCP_TOKEN_ENV));
            assert!(args.contains(GAMEENGINE_MCP_SERVER_NAME));
        }
        let claude = build_question_launch_plan(
            ExternalAgentProviderKind::ClaudeCode,
            &placement,
            "why is the player falling?",
            "http://127.0.0.1:1234/mcp",
        )
        .expect("Claude Code answers questions");
        let claude_args = claude
            .args
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        // A deny list is the rule provider settings cannot widen, so the
        // write-capable tools must be named on it rather than merely omitted.
        let denied = claude_args
            .iter()
            .position(|argument| argument == "--disallowedTools")
            .expect("write-capable tools are denied explicitly");
        for tool in ["Write", "Edit", "NotebookEdit", "Bash"] {
            assert!(
                claude_args[denied..]
                    .iter()
                    .any(|argument| argument == tool)
            );
        }
        let codex = build_question_launch_plan(
            ExternalAgentProviderKind::Codex,
            &placement,
            "why is the player falling?",
            "http://127.0.0.1:1234/mcp",
        )
        .expect("Codex answers questions");
        let codex_args = codex
            .args
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let feature_index = codex_args
            .iter()
            .position(|argument| argument == "--enable")
            .expect("Codex modern MCP feature flag");
        assert_eq!(
            codex_args.get(feature_index + 1).map(String::as_str),
            Some(CODEX_MCP_FEATURE)
        );
        assert!(codex_args.iter().any(|argument| argument == "read-only"));
        assert!(codex_args.iter().any(|argument| argument.contains(".url=")));
        assert!(
            codex_args
                .iter()
                .any(|argument| argument == "--ignore-user-config")
        );
    }

    #[test]
    fn a_stated_provider_failure_is_reported_instead_of_an_exit_code() {
        let codex_turn_failed = serde_json::json!({
            "type": "turn.failed",
            "error": { "message": "the model requires a newer Codex version" },
        })
        .to_string();
        assert_eq!(
            extract_error_text(ExternalAgentProviderKind::Codex, &codex_turn_failed),
            Some("the model requires a newer Codex version".to_owned())
        );
        let claude_expired = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": true,
            "result": "Failed to authenticate: OAuth session expired",
        })
        .to_string();
        assert_eq!(
            extract_error_text(ExternalAgentProviderKind::ClaudeCode, &claude_expired),
            Some("Failed to authenticate: OAuth session expired".to_owned())
        );
        let claude_answer = serde_json::json!({
            "type": "result",
            "is_error": false,
            "result": "Materials load through the asset manifest.",
        })
        .to_string();
        assert_eq!(
            extract_error_text(ExternalAgentProviderKind::ClaudeCode, &claude_answer),
            None
        );
        let message = question_failure_message(
            ExternalAgentProviderKind::Codex,
            ExternalAgentDiagnostics::default(),
            Some(1),
            Some("the model requires a newer Codex version"),
            &["{\"type\":\"turn.started\"}".to_owned()],
        );
        assert!(message.contains("the model requires a newer Codex version"));
        assert!(!message.contains("exited unsuccessfully"));
    }

    #[test]
    fn the_generic_provider_never_answers_a_question() {
        let placement = ExternalAgentExecutionPlacement::windows_native();
        let error = build_question_launch_plan(
            ExternalAgentProviderKind::Generic,
            &placement,
            "why is the player falling?",
            "http://127.0.0.1:1234/mcp",
        )
        .expect_err("a user-defined command cannot be proven read-only");
        assert!(error.contains("Generic command provider"));
    }

    #[test]
    fn a_question_prompt_keeps_recorded_turns_in_order() {
        let prompt = build_question_prompt(&[
            ExternalAgentQuestionTurn {
                role: ExternalAgentQuestionRole::User,
                text: "how do materials load?".to_owned(),
            },
            ExternalAgentQuestionTurn {
                role: ExternalAgentQuestionRole::Assistant,
                text: "  ".to_owned(),
            },
            ExternalAgentQuestionTurn {
                role: ExternalAgentQuestionRole::System,
                text: "inference failed".to_owned(),
            },
        ]);
        let first = prompt
            .find("User: how do materials load?")
            .expect("the user turn is rendered");
        let second = prompt
            .find("System: inference failed")
            .expect("the system turn is rendered");
        assert!(first < second);
        assert!(!prompt.contains("Assistant:"));
    }

    #[test]
    fn only_provider_assistant_text_becomes_an_answer() {
        let claude_result = serde_json::json!({
            "type": "result",
            "is_error": false,
            "result": "Materials load through the asset manifest.",
        })
        .to_string();
        assert_eq!(
            extract_answer_text(ExternalAgentProviderKind::ClaudeCode, &claude_result),
            Some("Materials load through the asset manifest.".to_owned())
        );
        let claude_error = serde_json::json!({
            "type": "result",
            "is_error": true,
            "result": "provider error",
        })
        .to_string();
        assert_eq!(
            extract_answer_text(ExternalAgentProviderKind::ClaudeCode, &claude_error),
            None
        );
        let codex_message = serde_json::json!({
            "type": "item.completed",
            "item": { "type": "agent_message", "text": "The controller clamps velocity." },
        })
        .to_string();
        assert_eq!(
            extract_answer_text(ExternalAgentProviderKind::Codex, &codex_message),
            Some("The controller clamps velocity.".to_owned())
        );
        let codex_command = serde_json::json!({
            "type": "item.completed",
            "item": { "type": "command_execution", "text": "ls" },
        })
        .to_string();
        assert_eq!(
            extract_answer_text(ExternalAgentProviderKind::Codex, &codex_command),
            None
        );
        assert_eq!(
            extract_answer_text(ExternalAgentProviderKind::ClaudeCode, "not json"),
            None
        );
    }

    #[test]
    fn a_sign_in_plan_starts_the_provider_owned_subscription_flow() {
        let placement = ExternalAgentExecutionPlacement::windows_native();
        let claude = build_sign_in_plan(ExternalAgentProviderKind::ClaudeCode, &placement)
            .expect("Claude Code owns an interactive sign-in");
        assert_eq!(claude.program, OsString::from("claude"));
        assert!(claude.args.contains(&OsString::from("--claudeai")));
        let codex = build_sign_in_plan(ExternalAgentProviderKind::Codex, &placement)
            .expect("Codex owns an interactive sign-in");
        assert_eq!(codex.program, OsString::from("codex"));
        assert!(codex.args.contains(&OsString::from("login")));
        assert!(build_sign_in_plan(ExternalAgentProviderKind::Generic, &placement).is_err());
    }

    #[test]
    fn an_install_plan_runs_the_provider_package_with_the_platform_launcher() {
        let native = build_install_plan(
            ExternalAgentProviderKind::Codex,
            &ExternalAgentExecutionPlacement::windows_native(),
        )
        .expect("Codex publishes an install package");
        // Process creation appends only `.exe`, so the Windows launcher name
        // has to carry its extension or the launch fails as "not found".
        assert_eq!(native.program, OsString::from("npm.cmd"));
        assert_eq!(
            native.args,
            vec![
                OsString::from("install"),
                OsString::from("-g"),
                OsString::from("@openai/codex@0.148.0"),
            ]
        );
        let wsl = build_install_plan(
            ExternalAgentProviderKind::ClaudeCode,
            &ExternalAgentExecutionPlacement {
                environment: ExternalAgentExecutionEnvironment::Wsl2Linux,
                distribution: "Ubuntu-24.04".to_owned(),
            },
        )
        .expect("Claude Code publishes an install package");
        assert_eq!(wsl.program, OsString::from(WSL_LAUNCHER));
        let args = wsl
            .args
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args[0], "-d");
        assert_eq!(args[1], "Ubuntu-24.04");
        assert_eq!(args[2], "--");
        assert_eq!(args[3], "npm");
        assert!(args.contains(&"@anthropic-ai/claude-code@2.1.237".to_owned()));
        assert!(
            build_install_plan(
                ExternalAgentProviderKind::Generic,
                &ExternalAgentExecutionPlacement::windows_native()
            )
            .is_err()
        );
    }

    #[test]
    fn the_command_a_setup_step_will_run_is_rendered_before_it_runs() {
        let placement = ExternalAgentExecutionPlacement::windows_native();
        let install = setup_command_text(
            ExternalAgentSetupAction::Install,
            ExternalAgentProviderKind::Codex,
            &placement,
        )
        .expect("an install command is shown");
        assert_eq!(install, "npm.cmd install -g @openai/codex@0.148.0");
        let sign_in = setup_command_text(
            ExternalAgentSetupAction::SignIn,
            ExternalAgentProviderKind::Codex,
            &placement,
        )
        .expect("a sign-in command is shown");
        assert_eq!(sign_in, "codex login");
        assert_eq!(
            setup_command_text(
                ExternalAgentSetupAction::Install,
                ExternalAgentProviderKind::Generic,
                &placement
            ),
            None
        );
    }

    #[test]
    fn a_copy_shadowed_by_another_path_directory_is_reported() {
        let report = |locations: &[&str]| ExternalAgentProviderReport {
            status: ExternalAgentProviderStatus::unchecked(ExternalAgentProviderKind::Codex),
            locations: locations.iter().map(|path| (*path).to_owned()).collect(),
            installer_available: true,
        };
        // A shim and its launcher in one directory are one installed copy.
        assert!(!report(&[r"C:\npm\codex", r"C:\npm\codex.cmd"]).has_shadowed_copies());
        assert!(report(&[r"C:\vendor\bin\codex.exe", r"C:\npm\codex.cmd"]).has_shadowed_copies());
        assert!(!report(&[]).has_shadowed_copies());
    }

    #[test]
    fn a_printed_sign_in_url_is_surfaced_without_surrounding_text() {
        assert_eq!(
            sign_in_url("Open this URL to authenticate: https://auth.example.com/x?y=1."),
            Some("https://auth.example.com/x?y=1".to_owned())
        );
        assert_eq!(sign_in_url("Waiting for the browser"), None);
    }
}
