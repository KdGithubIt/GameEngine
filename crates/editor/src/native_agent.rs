//! Provider-independent native AI question harness for AI Studio.
//!
//! The initial native slice is deliberately read-oriented. It retrieves current
//! GameEngine/project evidence, sends a bounded prompt to a user-selected local
//! model backend, and returns provenance plus measurable run metadata. It does
//! not acquire mutation permissions or participate in the write-capable
//! [`crate::agent_host::AgentRun`] state machine.

use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

pub(crate) const DEFAULT_LOCAL_MODEL_ENDPOINT: &str = "http://127.0.0.1:11434";
pub(crate) const BASELINE_HARNESS_VERSION: &str = "native-read-v1";
const BACKEND_ID: &str = "ollama-compatible";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_HTTP_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SOURCE_FILE_BYTES: u64 = 512 * 1024;
const MAX_SCANNED_FILES: usize = 1_200;
const SOURCE_CHUNK_CHARS: usize = 3_200;
const MAX_RETRIEVED_CHUNKS: usize = 8;
const MAX_CONVERSATION_MESSAGES: usize = 12;
const MAX_CONVERSATION_MESSAGE_CHARS: usize = 4_000;

#[derive(Debug)]
pub(crate) enum NativeAgentError {
    EmptyModel,
    InvalidEndpoint(String),
    BackendUnavailable(String),
    HttpStatus(u16, String),
    InvalidHttpResponse(String),
    ResponseTooLarge,
    EmptyResponse,
    WorkerDisconnected,
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for NativeAgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyModel => write!(formatter, "a local model name is required"),
            Self::InvalidEndpoint(message) => {
                write!(formatter, "invalid local model endpoint: {message}")
            }
            Self::BackendUnavailable(message) => {
                write!(formatter, "local model backend is unavailable: {message}")
            }
            Self::HttpStatus(status, message) => {
                write!(formatter, "local model backend returned HTTP {status}: {message}")
            }
            Self::InvalidHttpResponse(message) => write!(
                formatter,
                "local model backend returned an invalid HTTP response: {message}"
            ),
            Self::ResponseTooLarge => write!(
                formatter,
                "local model backend response exceeded the safety limit"
            ),
            Self::EmptyResponse => write!(formatter, "local model returned an empty answer"),
            Self::WorkerDisconnected => {
                write!(formatter, "native question worker disconnected unexpectedly")
            }
            Self::Io(error) => write!(formatter, "native agent I/O error: {error}"),
            Self::Json(error) => write!(formatter, "native agent JSON error: {error}"),
        }
    }
}

impl std::error::Error for NativeAgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for NativeAgentError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for NativeAgentError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LocalModelConfig {
    pub(crate) endpoint: String,
    pub(crate) model: String,
}

impl LocalModelConfig {
    pub(crate) fn capability_profile(&self) -> ModelCapabilityProfile {
        ModelCapabilityProfile {
            backend_id: BACKEND_ID,
            model_id: self.model.trim().to_owned(),
            structured_output: None,
            tool_use: None,
            image_input: None,
            reasoning: None,
            context_limit: None,
            benchmark_verified: false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ModelCapabilityProfile {
    pub(crate) backend_id: &'static str,
    pub(crate) model_id: String,
    pub(crate) structured_output: Option<bool>,
    pub(crate) tool_use: Option<bool>,
    pub(crate) image_input: Option<bool>,
    pub(crate) reasoning: Option<bool>,
    pub(crate) context_limit: Option<u64>,
    pub(crate) benchmark_verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuestionRole {
    User,
    Assistant,
    System,
}

impl QuestionRole {
    fn label(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Assistant => "Assistant",
            Self::System => "System",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct QuestionMessage {
    pub(crate) role: QuestionRole,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    EngineRepository,
    ProjectFile,
}

impl SourceKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::EngineRepository => "engine",
            Self::ProjectFile => "project",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RetrievedSource {
    pub(crate) kind: SourceKind,
    pub(crate) path: String,
}

#[derive(Debug, Clone)]
pub(crate) struct NativeMetrics {
    pub(crate) harness_version: &'static str,
    pub(crate) backend_id: &'static str,
    pub(crate) model_id: String,
    pub(crate) model_turns: u32,
    pub(crate) retrieval_chunks: usize,
    pub(crate) prompt_chars: usize,
    pub(crate) response_chars: usize,
    pub(crate) elapsed_ms: u64,
    pub(crate) prompt_eval_tokens: Option<u64>,
    pub(crate) response_tokens: Option<u64>,
    pub(crate) backend_duration_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct NativeAnswer {
    pub(crate) text: String,
    pub(crate) sources: Vec<RetrievedSource>,
    pub(crate) metrics: NativeMetrics,
}

pub(crate) struct NativeQuestionTask {
    result: Receiver<Result<NativeAnswer, NativeAgentError>>,
}

impl NativeQuestionTask {
    pub(crate) fn spawn(
        config: LocalModelConfig,
        project_root: PathBuf,
        conversation: Vec<QuestionMessage>,
    ) -> Result<Self, NativeAgentError> {
        if config.model.trim().is_empty() {
            return Err(NativeAgentError::EmptyModel);
        }
        // Validate before spawning so malformed/non-loopback endpoints fail at
        // the UI boundary instead of looking like a model timeout.
        LocalHttpEndpoint::parse(&config.endpoint)?;
        let (sender, result) = mpsc::channel();
        std::thread::Builder::new()
            .name("ai-native-question".to_owned())
            .spawn(move || {
                let answer = answer_question(&config, &project_root, &conversation);
                let _ = sender.send(answer);
            })?;
        Ok(Self { result })
    }

    pub(crate) fn poll(&self) -> Option<Result<NativeAnswer, NativeAgentError>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(NativeAgentError::WorkerDisconnected)),
        }
    }
}

#[derive(Debug, Clone)]
struct LocalHttpEndpoint {
    host: String,
    port: u16,
}

impl LocalHttpEndpoint {
    fn parse(value: &str) -> Result<Self, NativeAgentError> {
        let trimmed = value.trim().trim_end_matches('/');
        let authority = trimmed.strip_prefix("http://").ok_or_else(|| {
            NativeAgentError::InvalidEndpoint(
                "only plain HTTP loopback endpoints are supported in the initial local backend"
                    .to_owned(),
            )
        })?;
        if authority.is_empty()
            || authority.contains('/')
            || authority.contains('@')
            || authority.contains('?')
            || authority.contains('#')
        {
            return Err(NativeAgentError::InvalidEndpoint(
                "endpoint must contain only a loopback host and optional port".to_owned(),
            ));
        }
        let (host, port) = parse_authority(authority)?;
        if !is_loopback_host(&host) {
            return Err(NativeAgentError::InvalidEndpoint(
                "native local-model backends are restricted to loopback addresses".to_owned(),
            ));
        }
        Ok(Self { host, port })
    }

    fn connect(&self) -> Result<TcpStream, NativeAgentError> {
        let connect_host = self.host.trim_matches(|character| matches!(character, '[' | ']'));
        let addresses = (connect_host, self.port)
            .to_socket_addrs()
            .map_err(|error| NativeAgentError::BackendUnavailable(error.to_string()))?
            .filter(|address| address.ip().is_loopback())
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err(NativeAgentError::BackendUnavailable(
                "loopback endpoint did not resolve to a loopback socket".to_owned(),
            ));
        }
        let mut last_error = None;
        for address in addresses {
            match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(IO_TIMEOUT))?;
                    stream.set_write_timeout(Some(IO_TIMEOUT))?;
                    return Ok(stream);
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(NativeAgentError::BackendUnavailable(
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "connection failed".to_owned()),
        ))
    }

    fn host_header(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

fn parse_authority(authority: &str) -> Result<(String, u16), NativeAgentError> {
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest.find(']').ok_or_else(|| {
            NativeAgentError::InvalidEndpoint("malformed IPv6 loopback address".to_owned())
        })?;
        let host = format!("[{}]", &rest[..end]);
        let suffix = &rest[end + 1..];
        let port = if suffix.is_empty() {
            11434
        } else {
            suffix
                .strip_prefix(':')
                .ok_or_else(|| {
                    NativeAgentError::InvalidEndpoint("malformed IPv6 endpoint port".to_owned())
                })?
                .parse::<u16>()
                .map_err(|_| {
                    NativeAgentError::InvalidEndpoint("invalid endpoint port".to_owned())
                })?
        };
        return Ok((host, port));
    }
    let mut split = authority.rsplitn(2, ':');
    let last = split.next().unwrap_or_default();
    let first = split.next();
    match first {
        Some(host) if !host.contains(':') => {
            let port = last
                .parse::<u16>()
                .map_err(|_| NativeAgentError::InvalidEndpoint("invalid endpoint port".to_owned()))?;
            Ok((host.to_owned(), port))
        }
        Some(_) => Err(NativeAgentError::InvalidEndpoint(
            "IPv6 addresses must use bracket notation".to_owned(),
        )),
        None => Ok((last.to_owned(), 11434)),
    }
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.trim_matches(|character| matches!(character, '[' | ']'))
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn answer_question(
    config: &LocalModelConfig,
    project_root: &Path,
    conversation: &[QuestionMessage],
) -> Result<NativeAnswer, NativeAgentError> {
    let started = Instant::now();
    let query = conversation
        .iter()
        .rev()
        .find(|message| message.role == QuestionRole::User)
        .map(|message| message.text.as_str())
        .unwrap_or_default();
    let engine_root = discover_engine_root();
    let evidence = retrieve_evidence(query, engine_root.as_deref(), project_root)?;
    let prompt = build_prompt(conversation, &evidence);
    let backend = generate_local(config, &prompt)?;
    let text = backend.response.trim().to_owned();
    if text.is_empty() {
        return Err(NativeAgentError::EmptyResponse);
    }
    let sources = distinct_sources(&evidence);
    Ok(NativeAnswer {
        metrics: NativeMetrics {
            harness_version: BASELINE_HARNESS_VERSION,
            backend_id: BACKEND_ID,
            model_id: config.model.trim().to_owned(),
            model_turns: 1,
            retrieval_chunks: evidence.len(),
            prompt_chars: prompt.chars().count(),
            response_chars: text.chars().count(),
            elapsed_ms: duration_ms(started.elapsed()),
            prompt_eval_tokens: backend.prompt_eval_count,
            response_tokens: backend.eval_count,
            backend_duration_ms: backend.total_duration.map(|nanos| nanos / 1_000_000),
        },
        text,
        sources,
    })
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[derive(Debug, Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct GenerateResponse {
    response: String,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
    #[serde(default)]
    total_duration: Option<u64>,
}

fn generate_local(
    config: &LocalModelConfig,
    prompt: &str,
) -> Result<GenerateResponse, NativeAgentError> {
    let endpoint = LocalHttpEndpoint::parse(&config.endpoint)?;
    let body = serde_json::to_vec(&GenerateRequest {
        model: config.model.trim(),
        prompt,
        stream: false,
    })?;
    let mut stream = endpoint.connect()?;
    write!(
        stream,
        "POST /api/generate HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        endpoint.host_header(),
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()?;

    let mut response = Vec::new();
    stream
        .take(MAX_HTTP_RESPONSE_BYTES + 1)
        .read_to_end(&mut response)?;
    if response.len() as u64 > MAX_HTTP_RESPONSE_BYTES {
        return Err(NativeAgentError::ResponseTooLarge);
    }
    let parsed = parse_http_response(&response)?;
    if parsed.status != 200 {
        let message = String::from_utf8_lossy(&parsed.body);
        return Err(NativeAgentError::HttpStatus(
            parsed.status,
            truncate_text(message.trim(), 500),
        ));
    }
    serde_json::from_slice(&parsed.body).map_err(Into::into)
}

struct ParsedHttpResponse {
    status: u16,
    body: Vec<u8>,
}

fn parse_http_response(bytes: &[u8]) -> Result<ParsedHttpResponse, NativeAgentError> {
    let header_end = find_bytes(bytes, b"\r\n\r\n").ok_or_else(|| {
        NativeAgentError::InvalidHttpResponse("header terminator is missing".to_owned())
    })?;
    let headers = std::str::from_utf8(&bytes[..header_end]).map_err(|_| {
        NativeAgentError::InvalidHttpResponse("headers are not valid UTF-8".to_owned())
    })?;
    let mut lines = headers.split("\r\n");
    let status_line = lines.next().ok_or_else(|| {
        NativeAgentError::InvalidHttpResponse("status line is missing".to_owned())
    })?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| {
            NativeAgentError::InvalidHttpResponse("status code is missing".to_owned())
        })?
        .parse::<u16>()
        .map_err(|_| {
            NativeAgentError::InvalidHttpResponse("status code is invalid".to_owned())
        })?;
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(value.trim().parse::<usize>().map_err(|_| {
                NativeAgentError::InvalidHttpResponse("Content-Length is invalid".to_owned())
            })?);
        }
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value
                .split(',')
                .any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"))
        {
            chunked = true;
        }
    }
    let raw_body = &bytes[header_end + 4..];
    let body = if chunked {
        decode_chunked(raw_body)?
    } else if let Some(length) = content_length {
        if raw_body.len() < length {
            return Err(NativeAgentError::InvalidHttpResponse(
                "response body is shorter than Content-Length".to_owned(),
            ));
        }
        raw_body[..length].to_vec()
    } else {
        raw_body.to_vec()
    };
    Ok(ParsedHttpResponse { status, body })
}

fn decode_chunked(mut bytes: &[u8]) -> Result<Vec<u8>, NativeAgentError> {
    let mut output = Vec::new();
    loop {
        let line_end = find_bytes(bytes, b"\r\n").ok_or_else(|| {
            NativeAgentError::InvalidHttpResponse("chunk size terminator is missing".to_owned())
        })?;
        let size_text = std::str::from_utf8(&bytes[..line_end]).map_err(|_| {
            NativeAgentError::InvalidHttpResponse("chunk size is not UTF-8".to_owned())
        })?;
        let size = usize::from_str_radix(
            size_text.split(';').next().unwrap_or_default().trim(),
            16,
        )
        .map_err(|_| {
            NativeAgentError::InvalidHttpResponse("chunk size is invalid".to_owned())
        })?;
        bytes = &bytes[line_end + 2..];
        if size == 0 {
            return Ok(output);
        }
        if bytes.len() < size + 2 || &bytes[size..size + 2] != b"\r\n" {
            return Err(NativeAgentError::InvalidHttpResponse(
                "chunk payload is truncated".to_owned(),
            ));
        }
        output.extend_from_slice(&bytes[..size]);
        if output.len() as u64 > MAX_HTTP_RESPONSE_BYTES {
            return Err(NativeAgentError::ResponseTooLarge);
        }
        bytes = &bytes[size + 2..];
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

#[derive(Debug, Clone)]
struct EvidenceChunk {
    source: RetrievedSource,
    text: String,
    score: usize,
}

fn retrieve_evidence(
    query: &str,
    engine_root: Option<&Path>,
    project_root: &Path,
) -> Result<Vec<EvidenceChunk>, NativeAgentError> {
    let query_terms = query_terms(query);
    let mut candidates = Vec::new();
    let mut engine_scanned = 0;
    if let Some(root) = engine_root {
        scan_path(
            root,
            root,
            Path::new("AGENTS.md"),
            SourceKind::EngineRepository,
            &query_terms,
            &mut candidates,
            &mut engine_scanned,
        )?;
        scan_path(
            root,
            root,
            Path::new("docs"),
            SourceKind::EngineRepository,
            &query_terms,
            &mut candidates,
            &mut engine_scanned,
        )?;
        scan_path(
            root,
            root,
            Path::new("crates"),
            SourceKind::EngineRepository,
            &query_terms,
            &mut candidates,
            &mut engine_scanned,
        )?;
    }
    let mut project_scanned = 0;
    for relative in ["project.json", "game", "assets"] {
        scan_path(
            project_root,
            project_root,
            Path::new(relative),
            SourceKind::ProjectFile,
            &query_terms,
            &mut candidates,
            &mut project_scanned,
        )?;
    }
    candidates.sort_by_key(|chunk| Reverse((chunk.score, chunk.source.path.clone())));
    candidates.truncate(MAX_RETRIEVED_CHUNKS);
    Ok(candidates)
}

fn scan_path(
    root: &Path,
    base: &Path,
    relative: &Path,
    kind: SourceKind,
    query_terms: &BTreeSet<String>,
    output: &mut Vec<EvidenceChunk>,
    scanned: &mut usize,
) -> Result<(), NativeAgentError> {
    if *scanned >= MAX_SCANNED_FILES {
        return Ok(());
    }
    let path = root.join(relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        let mut entries = fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if *scanned >= MAX_SCANNED_FILES {
                break;
            }
            let name = entry.file_name();
            if matches!(name.to_str(), Some(".git" | "target" | "node_modules" | ".gameengine")) {
                continue;
            }
            let child = relative.join(name);
            scan_path(root, base, &child, kind, query_terms, output, scanned)?;
        }
        return Ok(());
    }
    if !metadata.is_file()
        || metadata.len() > MAX_SOURCE_FILE_BYTES
        || !is_retrieval_text_file(&path)
    {
        return Ok(());
    }
    *scanned += 1;
    let bytes = fs::read(&path)?;
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(());
    };
    let relative_path = path
        .strip_prefix(base)
        .unwrap_or(path.as_path())
        .to_string_lossy()
        .replace('\\', "/");
    for chunk in chunk_text(&text) {
        let score = score_chunk(query_terms, &relative_path, &chunk);
        if score == 0 && !query_terms.is_empty() {
            continue;
        }
        output.push(EvidenceChunk {
            source: RetrievedSource {
                kind,
                path: relative_path.clone(),
            },
            text: chunk,
            score,
        });
    }
    Ok(())
}

fn is_retrieval_text_file(path: &Path) -> bool {
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if matches!(file_name, "Cargo.toml" | "Cargo.lock" | "project.json" | "AGENTS.md") {
        return true;
    }
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("md" | "rs" | "toml" | "json" | "ron" | "rhai" | "txt" | "yaml" | "yml")
    )
}

fn query_terms(query: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    let mut current = String::new();
    let mut current_ascii = None;
    let flush = |current: &mut String, terms: &mut BTreeSet<String>| {
        if current.chars().count() >= 2 {
            terms.insert(current.to_lowercase());
        }
        current.clear();
    };
    for character in query.chars() {
        let is_word = character.is_alphanumeric() || character == '_';
        if !is_word {
            flush(&mut current, &mut terms);
            current_ascii = None;
            continue;
        }
        let is_ascii = character.is_ascii_alphanumeric() || character == '_';
        if current_ascii.is_some_and(|previous| previous != is_ascii) {
            flush(&mut current, &mut terms);
        }
        current.push(character);
        current_ascii = Some(is_ascii);
    }
    flush(&mut current, &mut terms);
    terms
}

fn score_chunk(query_terms: &BTreeSet<String>, path: &str, text: &str) -> usize {
    if query_terms.is_empty() {
        return usize::from(path.ends_with("AGENTS.md"));
    }
    let path = path.to_lowercase();
    let text = text.to_lowercase();
    query_terms
        .iter()
        .map(|term| {
            let path_hits = path.matches(term.as_str()).count();
            let text_hits = text.matches(term.as_str()).count().min(8);
            path_hits.saturating_mul(6).saturating_add(text_hits)
        })
        .sum()
}

fn chunk_text(text: &str) -> Vec<String> {
    if text.chars().count() <= SOURCE_CHUNK_CHARS {
        return vec![text.to_owned()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if current.chars().count() + line.chars().count() + 1 > SOURCE_CHUNK_CHARS
            && !current.is_empty()
        {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn distinct_sources(evidence: &[EvidenceChunk]) -> Vec<RetrievedSource> {
    let mut seen = BTreeSet::new();
    let mut sources = Vec::new();
    for chunk in evidence {
        let key = (chunk.source.kind.label(), chunk.source.path.clone());
        if seen.insert(key) {
            sources.push(chunk.source.clone());
        }
    }
    sources
}

fn build_prompt(conversation: &[QuestionMessage], evidence: &[EvidenceChunk]) -> String {
    let mut prompt = String::from(concat!(
        "You are the native read-oriented GameEngine AI Studio assistant.\n",
        "This request is a question/learning turn, not a write-capable AgentRun.\n",
        "Do not claim to edit files, run mutation tools, or acquire write permissions.\n",
        "For GameEngine-specific facts, retrieved repository/project evidence is authoritative ",
        "over model memory.\n",
        "Cite GameEngine evidence inline as [engine:path] or [project:path].\n",
        "If the evidence does not support a GameEngine-specific claim, say that it is not ",
        "established by the current sources.\n",
        "General game-development knowledge may be used, but distinguish it from ",
        "repository/project facts.\n",
        "Answer the user's latest question directly and keep useful conversation context.\n\n",
    ));
    prompt.push_str("Retrieved evidence:\n");
    if evidence.is_empty() {
        prompt.push_str("(No matching repository/project evidence was found.)\n");
    } else {
        for chunk in evidence {
            prompt.push_str(&format!(
                "\n--- [{}:{}] ---\n{}\n",
                chunk.source.kind.label(),
                chunk.source.path,
                chunk.text
            ));
        }
    }
    prompt.push_str("\nConversation:\n");
    for message in conversation.iter().rev().take(MAX_CONVERSATION_MESSAGES).rev() {
        prompt.push_str(message.role.label());
        prompt.push_str(": ");
        prompt.push_str(&truncate_text(
            message.text.trim(),
            MAX_CONVERSATION_MESSAGE_CHARS,
        ));
        prompt.push('\n');
    }
    prompt.push_str("Assistant: ");
    prompt
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut output = text.chars().take(max_chars).collect::<String>();
    output.push_str("…");
    output
}

fn discover_engine_root() -> Option<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(current_dir) = std::env::current_dir() {
        starts.push(current_dir);
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        starts.push(parent.to_path_buf());
    }
    for start in starts {
        for ancestor in start.ancestors() {
            if ancestor.join("AGENTS.md").is_file()
                && ancestor.join("docs/AI_FRIENDLY_AUTHORING_SPEC.md").is_file()
                && ancestor.join("crates/editor/Cargo.toml").is_file()
            {
                return Some(ancestor.to_path_buf());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gameengine-native-agent-{name}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn local_endpoint_accepts_only_loopback_http() {
        assert!(LocalHttpEndpoint::parse("http://127.0.0.1:11434").is_ok());
        assert!(LocalHttpEndpoint::parse("http://localhost:11434/").is_ok());
        assert!(LocalHttpEndpoint::parse("http://[::1]:11434").is_ok());
        assert!(LocalHttpEndpoint::parse("https://127.0.0.1:11434").is_err());
        assert!(LocalHttpEndpoint::parse("http://192.168.1.2:11434").is_err());
        assert!(LocalHttpEndpoint::parse("http://localhost:11434/api").is_err());
    }

    #[test]
    fn http_parser_handles_content_length_and_chunked_body() {
        let content_length = b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
        let parsed = parse_http_response(content_length).expect("content-length response");
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.body, b"{\"a\":1}");

        let chunked = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Transfer-Encoding: chunked\r\n\r\n",
            "4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"
        );
        let parsed = parse_http_response(chunked.as_bytes()).expect("chunked response");
        assert_eq!(parsed.body, b"Wikipedia");
    }

    #[test]
    fn retrieval_keeps_paths_relative_and_prefers_matching_project_source() {
        let project = temp_path("retrieval");
        let _ = fs::remove_dir_all(&project);
        fs::create_dir_all(project.join("game/src")).expect("project source directory");
        fs::write(
            project.join("game/src/player.rs"),
            "pub struct PlayerHealth { pub current: u32 }\n",
        )
        .expect("player source");
        fs::write(
            project.join("game/src/unrelated.rs"),
            "pub struct CameraSettings;\n",
        )
        .expect("unrelated source");

        let evidence = retrieve_evidence("PlayerHealth current", None, &project)
            .expect("retrieval evidence");
        assert!(!evidence.is_empty());
        assert_eq!(evidence[0].source.kind, SourceKind::ProjectFile);
        assert_eq!(evidence[0].source.path, "game/src/player.rs");
        assert!(!evidence[0].source.path.contains(project.to_string_lossy().as_ref()));
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn query_terms_keep_ascii_identifiers_inside_japanese_text() {
        let terms = query_terms("アニメーションfadeはどこで決まる？");
        assert!(terms.contains("fade"));
    }

    #[test]
    fn read_harness_prompt_forbids_silent_mutation() {
        let conversation = vec![QuestionMessage {
            role: QuestionRole::User,
            text: "How does this work?".to_owned(),
        }];
        let prompt = build_prompt(&conversation, &[]);
        assert!(prompt.contains("not a write-capable AgentRun"));
        assert!(prompt.contains("Do not claim to edit files"));
        assert!(prompt.contains("not established by the current sources"));
    }
}
