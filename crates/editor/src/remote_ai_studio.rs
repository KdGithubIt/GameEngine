//! Loopback-only Remote AI Studio companion transport.
//!
//! The transport is intentionally a presentation/application boundary over the
//! existing Editor-owned Agent Host. It never exposes MCP, shell, filesystem,
//! Git, process-launch, or renderer/model resource controls.

use crate::agent_host::{
    AgentCapability, AgentEvent, AgentEventEvidence, AgentEventKind, AgentHost, AgentRun,
    AgentRunState, AgentSession,
};
use crate::live_observation::DEFAULT_LIVE_OBSERVATION_FPS;
use eframe::egui;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const HOST_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_IDEMPOTENCY_ENTRIES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemotePermissionScope {
    Once,
    Run,
    Project,
    Deny,
}

#[derive(Debug, Clone)]
pub(crate) enum RemoteOperation {
    Sessions,
    Snapshot {
        session_id: String,
    },
    Message {
        session_id: String,
        request_id: String,
        text: String,
    },
    Go {
        session_id: String,
        request_id: String,
        proposal_version: u64,
    },
    Stop {
        run_id: String,
        request_id: String,
    },
    AwaitingUser {
        run_id: String,
        request_id: String,
        text: String,
    },
    Permission {
        run_id: String,
        request_id: String,
        capability: AgentCapability,
        scope: RemotePermissionScope,
    },
    Events {
        run_id: String,
        after: u64,
    },
    Frame {
        run_id: String,
        artifact_id: String,
    },
    StartLiveObservation {
        run_id: String,
        request_id: String,
        max_fps: u8,
    },
    LiveObservationStatus {
        media_session_id: String,
        media_token: String,
    },
    LiveObservationFrame {
        media_session_id: String,
        media_token: String,
        sequence: u64,
    },
    StopLiveObservation {
        media_session_id: String,
        media_token: String,
        request_id: String,
    },
}

impl RemoteOperation {
    fn request_identity(&self) -> Option<(&str, String)> {
        match self {
            Self::Message {
                session_id,
                request_id,
                text,
            } => Some((request_id, format!("message:{session_id}:{text}"))),
            Self::Go {
                session_id,
                request_id,
                proposal_version,
            } => Some((
                request_id,
                format!("go:{session_id}:{proposal_version}"),
            )),
            Self::Stop { run_id, request_id } => {
                Some((request_id, format!("stop:{run_id}")))
            }
            Self::AwaitingUser {
                run_id,
                request_id,
                text,
            } => Some((
                request_id,
                format!("awaiting_user:{run_id}:{text}"),
            )),
            Self::Permission {
                run_id,
                request_id,
                capability,
                scope,
            } => Some((
                request_id,
                format!("permission:{run_id}:{capability:?}:{scope:?}"),
            )),
            Self::StartLiveObservation {
                run_id,
                request_id,
                max_fps,
            } => Some((request_id, format!("live_start:{run_id}:{max_fps}"))),
            Self::StopLiveObservation {
                media_session_id,
                media_token,
                request_id,
            } => Some((
                request_id,
                format!("live_stop:{media_session_id}:{media_token}"),
            )),
            Self::Sessions
            | Self::Snapshot { .. }
            | Self::Events { .. }
            | Self::Frame { .. }
            | Self::LiveObservationStatus { .. }
            | Self::LiveObservationFrame { .. } => None
        }
    }
}

pub(crate) struct RemoteAiStudioRequest {
    operation: RemoteOperation,
    response: mpsc::SyncSender<RemoteAiStudioResponse>,
}

impl RemoteAiStudioRequest {
    pub(crate) fn operation(&self) -> &RemoteOperation {
        &self.operation
    }

    pub(crate) fn respond(self, response: RemoteAiStudioResponse) {
        let _ = self.response.send(response);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteAiStudioResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl RemoteAiStudioResponse {
    pub(crate) fn json(value: Value) -> Self {
        match serde_json::to_vec(&value) {
            Ok(body) => Self {
                status: 200,
                content_type: "application/json; charset=utf-8",
                body,
            },
            Err(_) => Self::error(
                500,
                "serialization_failed",
                "The remote response could not be serialized.",
                false,
            ),
        }
    }

    pub(crate) fn sse(value: Value) -> Self {
        let data = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
        Self {
            status: 200,
            content_type: "text/event-stream; charset=utf-8",
            body: format!("event: agent_events\ndata: {data}\n\n").into_bytes(),
        }
    }

    pub(crate) fn png(bytes: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: "image/png",
            body: bytes,
        }
    }

    pub(crate) fn error(
        status: u16,
        category: &str,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        let body = serde_json::to_vec(&json!({
            "error": {
                "category": category,
                "message": sanitize_text(&message.into()),
                "retryable": retryable,
            }
        }))
        .unwrap_or_else(|_| b"{\"error\":{\"category\":\"serialization_failed\",\"message\":\"Remote error\",\"retryable\":false}}".to_vec());
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body,
        }
    }
}

#[derive(Debug)]
pub(crate) enum RemoteAiStudioServerError {
    Bind(io::Error),
    Configure(io::Error),
    Random(String),
    Thread(io::Error),
}

impl fmt::Display for RemoteAiStudioServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(error) => write!(formatter, "could not bind Remote AI Studio loopback gateway: {error}"),
            Self::Configure(error) => write!(formatter, "could not configure Remote AI Studio gateway: {error}"),
            Self::Random(error) => write!(formatter, "could not create Remote AI Studio credential: {error}"),
            Self::Thread(error) => write!(formatter, "could not start Remote AI Studio listener thread: {error}"),
        }
    }
}

impl std::error::Error for RemoteAiStudioServerError {}

pub(crate) struct RemoteAiStudioServer {
    local_addr: SocketAddr,
    endpoint: String,
    access_token: String,
    shutdown: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<()>>,
}

impl RemoteAiStudioServer {
    pub(crate) fn start(
        context: egui::Context,
    ) -> Result<(Self, mpsc::Receiver<RemoteAiStudioRequest>), RemoteAiStudioServerError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(RemoteAiStudioServerError::Bind)?;
        listener
            .set_nonblocking(true)
            .map_err(RemoteAiStudioServerError::Configure)?;
        let local_addr = listener
            .local_addr()
            .map_err(RemoteAiStudioServerError::Configure)?;
        let access_token = new_access_token()?;
        let endpoint = format!("http://{local_addr}");
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_token = access_token.clone();
        let (request_sender, request_receiver) = mpsc::channel();
        let listener_thread = thread::Builder::new()
            .name("gameengine-remote-ai-studio".into())
            .spawn(move || {
                serve_loop(
                    listener,
                    worker_token,
                    request_sender,
                    context,
                    worker_shutdown,
                );
            })
            .map_err(RemoteAiStudioServerError::Thread)?;
        Ok((
            Self {
                local_addr,
                endpoint,
                access_token,
                shutdown,
                listener_thread: Some(listener_thread),
            },
            request_receiver,
        ))
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) fn companion_url(&self) -> String {
        format!("{}/#access_token={}", self.endpoint, self.access_token)
    }
}

impl Drop for RemoteAiStudioServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.local_addr, Duration::from_millis(100));
        if let Some(thread) = self.listener_thread.take() {
            let _ = thread.join();
        }
    }
}

fn new_access_token() -> Result<String, RemoteAiStudioServerError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| RemoteAiStudioServerError::Random(error.to_string()))?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut token, "{byte:02x}")
            .expect("writing hexadecimal text into String cannot fail");
    }
    Ok(token)
}

#[derive(Clone)]
struct CachedMutation {
    fingerprint: String,
    response: RemoteAiStudioResponse,
}

#[derive(Default)]
struct MutationCache {
    entries: BTreeMap<String, CachedMutation>,
    order: VecDeque<String>,
}

impl MutationCache {
    fn execute<F>(&mut self, operation: &RemoteOperation, execute: F) -> RemoteAiStudioResponse
    where
        F: FnOnce() -> RemoteAiStudioResponse,
    {
        let Some((request_id, fingerprint)) = operation.request_identity() else {
            return execute();
        };
        if let Some(cached) = self.entries.get(request_id) {
            if cached.fingerprint == fingerprint {
                return cached.response.clone();
            }
            return RemoteAiStudioResponse::error(
                409,
                "idempotency_conflict",
                "The request identity was already used for a different mutation.",
                false,
            );
        }
        let response = execute();
        self.entries.insert(
            request_id.to_owned(),
            CachedMutation {
                fingerprint,
                response: response.clone(),
            },
        );
        self.order.push_back(request_id.to_owned());
        while self.order.len() > MAX_IDEMPOTENCY_ENTRIES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        response
    }
}

fn serve_loop(
    listener: TcpListener,
    access_token: String,
    request_sender: mpsc::Sender<RemoteAiStudioRequest>,
    context: egui::Context,
    shutdown: Arc<AtomicBool>,
) {
    let mut cache = MutationCache::default();
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                let _ = handle_connection(
                    stream,
                    &access_token,
                    &request_sender,
                    &context,
                    &mut cache,
                );
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    access_token: &str,
    request_sender: &mpsc::Sender<RemoteAiStudioRequest>,
    context: &egui::Context,
    cache: &mut MutationCache,
) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let request = match read_http_request(&mut stream) {
        Ok(request) => request,
        Err(status) => {
            return write_response(
                &mut stream,
                RemoteAiStudioResponse::error(status, "invalid_request", "Invalid HTTP request.", false),
            );
        }
    };

    let path = request.path.split('?').next().unwrap_or(request.path.as_str());
    if request.method == "GET" && path == "/" {
        return write_response(
            &mut stream,
            RemoteAiStudioResponse {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: COMPANION_HTML.as_bytes().to_vec(),
            },
        );
    }
    if !path.starts_with("/api/") {
        return write_response(
            &mut stream,
            RemoteAiStudioResponse::error(404, "not_found", "Endpoint not found.", false),
        );
    }
    let expected = format!("Bearer {access_token}");
    if request.headers.get("authorization") != Some(&expected) {
        return write_response(
            &mut stream,
            RemoteAiStudioResponse::error(401, "unauthorized", "Remote authentication is required.", false),
        );
    }
    let operation = match route_request(&request) {
        Ok(operation) => operation,
        Err(response) => return write_response(&mut stream, response),
    };
    let response = cache.execute(&operation, || {
        dispatch_to_editor(operation.clone(), request_sender, context)
    });
    write_response(&mut stream, response)
}

fn dispatch_to_editor(
    operation: RemoteOperation,
    request_sender: &mpsc::Sender<RemoteAiStudioRequest>,
    context: &egui::Context,
) -> RemoteAiStudioResponse {
    let (response_sender, response_receiver) = mpsc::sync_channel(1);
    if request_sender
        .send(RemoteAiStudioRequest {
            operation,
            response: response_sender,
        })
        .is_err()
    {
        return RemoteAiStudioResponse::error(
            503,
            "host_unavailable",
            "The Editor Agent Host is unavailable.",
            true,
        );
    }
    context.request_repaint();
    match response_receiver.recv_timeout(HOST_RESPONSE_TIMEOUT) {
        Ok(response) => response,
        Err(mpsc::RecvTimeoutError::Timeout) => RemoteAiStudioResponse::error(
            504,
            "host_timeout",
            "The Editor Agent Host did not answer in time.",
            true,
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => RemoteAiStudioResponse::error(
            503,
            "host_unavailable",
            "The Editor Agent Host disconnected.",
            true,
        ),
    }
}

#[derive(Deserialize)]
struct MessageBody {
    request_id: String,
    text: String,
}

#[derive(Deserialize)]
struct GoBody {
    request_id: String,
    proposal_version: u64,
}

#[derive(Deserialize)]
struct RequestIdBody {
    request_id: String,
}

#[derive(Deserialize)]
struct AwaitingUserBody {
    request_id: String,
    text: String,
}

#[derive(Deserialize)]
struct PermissionBody {
    request_id: String,
    capability: AgentCapability,
    scope: RemotePermissionScope,
}

#[derive(Deserialize)]
struct StartLiveObservationBody {
    request_id: String,
    #[serde(default = "default_live_observation_fps")]
    max_fps: u8,
}

fn default_live_observation_fps() -> u8 {
    DEFAULT_LIVE_OBSERVATION_FPS
}

fn route_request(request: &HttpRequest) -> Result<RemoteOperation, RemoteAiStudioResponse> {
    let path = request.path.split('?').next().unwrap_or(request.path.as_str());
    let parts = path
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    match (request.method.as_str(), parts.as_slice()) {
        ("GET", ["api", "sessions"]) => Ok(RemoteOperation::Sessions),
        ("GET", ["api", "sessions", session_id]) => Ok(RemoteOperation::Snapshot {
            session_id: (*session_id).to_owned(),
        }),
        ("POST", ["api", "sessions", session_id, "messages"]) => {
            let body: MessageBody = parse_json_body(request)?;
            validate_request_id(&body.request_id)?;
            if body.text.trim().is_empty() {
                return Err(RemoteAiStudioResponse::error(
                    400,
                    "invalid_message",
                    "Conversation message must not be empty.",
                    false,
                ));
            }
            Ok(RemoteOperation::Message {
                session_id: (*session_id).to_owned(),
                request_id: body.request_id,
                text: body.text,
            })
        }
        ("POST", ["api", "sessions", session_id, "go"]) => {
            let body: GoBody = parse_json_body(request)?;
            validate_request_id(&body.request_id)?;
            Ok(RemoteOperation::Go {
                session_id: (*session_id).to_owned(),
                request_id: body.request_id,
                proposal_version: body.proposal_version,
            })
        }
        ("POST", ["api", "runs", run_id, "stop"]) => {
            let body: RequestIdBody = parse_json_body(request)?;
            validate_request_id(&body.request_id)?;
            Ok(RemoteOperation::Stop {
                run_id: (*run_id).to_owned(),
                request_id: body.request_id,
            })
        }
        ("POST", ["api", "runs", run_id, "awaiting-user"]) => {
            let body: AwaitingUserBody = parse_json_body(request)?;
            validate_request_id(&body.request_id)?;
            if body.text.trim().is_empty() {
                return Err(RemoteAiStudioResponse::error(
                    400,
                    "invalid_response",
                    "AwaitingUser response must not be empty.",
                    false,
                ));
            }
            Ok(RemoteOperation::AwaitingUser {
                run_id: (*run_id).to_owned(),
                request_id: body.request_id,
                text: body.text,
            })
        }
        ("POST", ["api", "runs", run_id, "permissions"]) => {
            let body: PermissionBody = parse_json_body(request)?;
            validate_request_id(&body.request_id)?;
            Ok(RemoteOperation::Permission {
                run_id: (*run_id).to_owned(),
                request_id: body.request_id,
                capability: body.capability,
                scope: body.scope,
            })
        }
        ("GET", ["api", "runs", run_id, "events"]) => Ok(RemoteOperation::Events {
            run_id: (*run_id).to_owned(),
            after: query_u64(&request.path, "after").unwrap_or(0),
        }),
        ("GET", ["api", "runs", run_id, "frames", artifact_id]) => {
            Ok(RemoteOperation::Frame {
                run_id: (*run_id).to_owned(),
                artifact_id: (*artifact_id).to_owned(),
            })
        }
        ("POST", ["api", "runs", run_id, "live"]) => {
            let body: StartLiveObservationBody = parse_json_body(request)?;
            validate_request_id(&body.request_id)?;
            Ok(RemoteOperation::StartLiveObservation {
                run_id: (*run_id).to_owned(),
                request_id: body.request_id,
                max_fps: body.max_fps,
            })
        }
        ("GET", ["api", "live", media_session_id]) => {
            Ok(RemoteOperation::LiveObservationStatus {
                media_session_id: (*media_session_id).to_owned(),
                media_token: request
                    .headers
                    .get("x-gameengine-media-token")
                    .cloned()
                    .unwrap_or_default(),
            })
        }
        ("GET", ["api", "live", media_session_id, "frames", sequence]) => {
            let sequence = sequence.parse::<u64>().map_err(|_| {
                RemoteAiStudioResponse::error(
                    400,
                    "invalid_sequence",
                    "Live observation frame sequence must be an unsigned integer.",
                    false,
                )
            })?;
            Ok(RemoteOperation::LiveObservationFrame {
                media_session_id: (*media_session_id).to_owned(),
                media_token: request
                    .headers
                    .get("x-gameengine-media-token")
                    .cloned()
                    .unwrap_or_default(),
                sequence,
            })
        }
        ("POST", ["api", "live", media_session_id, "stop"]) => {
            let body: RequestIdBody = parse_json_body(request)?;
            validate_request_id(&body.request_id)?;
            Ok(RemoteOperation::StopLiveObservation {
                media_session_id: (*media_session_id).to_owned(),
                media_token: request
                    .headers
                    .get("x-gameengine-media-token")
                    .cloned()
                    .unwrap_or_default(),
                request_id: body.request_id,
            })
        }
        _ => Err(RemoteAiStudioResponse::error(
            404,
            "not_found",
            "Endpoint not found.",
            false,
        )),
    }
}

fn validate_request_id(request_id: &str) -> Result<(), RemoteAiStudioResponse> {
    if request_id.is_empty()
        || request_id.len() > 128
        || !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(RemoteAiStudioResponse::error(
            400,
            "invalid_request_id",
            "Request identity must be 1-128 safe ASCII characters.",
            false,
        ));
    }
    Ok(())
}

fn parse_json_body<T: for<'de> Deserialize<'de>>(
    request: &HttpRequest,
) -> Result<T, RemoteAiStudioResponse> {
    if !request
        .headers
        .get("content-type")
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"))
    {
        return Err(RemoteAiStudioResponse::error(
            415,
            "content_type",
            "application/json is required.",
            false,
        ));
    }
    serde_json::from_slice(&request.body).map_err(|_| {
        RemoteAiStudioResponse::error(400, "invalid_json", "Invalid JSON request body.", false)
    })
}

fn query_u64(path: &str, key: &str) -> Option<u64> {
    let query = path.split_once('?')?.1;
    query.split('&').find_map(|item| {
        let (name, value) = item.split_once('=')?;
        (name == key).then(|| value.parse::<u64>().ok()).flatten()
    })
}

struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, u16> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).map_err(|_| 400u16)?;
        if read == 0 {
            return Err(400);
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > MAX_HEADER_BYTES + MAX_BODY_BYTES {
            return Err(413);
        }
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if bytes.len() > MAX_HEADER_BYTES {
            return Err(431);
        }
    };
    let header_text = std::str::from_utf8(&bytes[..header_end]).map_err(|_| 400u16)?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().ok_or(400u16)?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().ok_or(400u16)?.to_owned();
    let path = request_parts.next().ok_or(400u16)?.to_owned();
    if request_parts.next().is_none() || request_parts.next().is_some() {
        return Err(400);
    }
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(400u16)?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    if headers.contains_key("transfer-encoding") {
        return Err(501);
    }
    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>().map_err(|_| 400u16))
        .transpose()?
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return Err(413);
    }
    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut buffer).map_err(|_| 400u16)?;
        if read == 0 {
            return Err(400);
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > header_end + MAX_BODY_BYTES {
            return Err(413);
        }
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

fn write_response(stream: &mut TcpStream, response: RemoteAiStudioResponse) -> io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src 'self' blob: data:; connect-src 'self'\r\nConnection: close\r\n\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    )?;
    stream.write_all(&response.body)
}

pub(crate) fn sessions_json(host: &AgentHost, project_id: &str) -> Value {
    json!({
        "project_id": project_id,
        "sessions": host.session_ids().into_iter().filter_map(|id| {
            host.session(&id).ok().map(|session| json!({
                "id": session.id,
                "title": sanitize_text(&session.title),
                "proposal_version": session.proposal.version,
                "active_run_id": session.runs.iter().rev().find(|run| !is_terminal(run.state)).map(|run| run.id.as_str()),
            }))
        }).collect::<Vec<_>>()
    })
}

pub(crate) fn snapshot_json(
    host: &AgentHost,
    project_id: &str,
    session_id: &str,
    pending_permission: Option<(&str, AgentCapability)>,
) -> Result<Value, String> {
    let session = host.session(session_id).map_err(|error| error.to_string())?;
    let active_run = session.runs.iter().rev().find(|run| !is_terminal(run.state));
    Ok(json!({
        "project_id": project_id,
        "session": session_json(session),
        "active_run": active_run.map(run_json),
        "pending_permission": pending_permission.map(|(run_id, capability)| json!({
            "run_id": run_id,
            "capability": serde_json::to_value(capability).unwrap_or(Value::Null),
            "label": capability.label(),
        })),
        "awaiting_user": active_run.filter(|run| run.state == AgentRunState::AwaitingUser).map(|run| json!({
            "run_id": run.id,
            "message": latest_safe_event_message(run),
        })),
    }))
}

pub(crate) fn events_json(host: &AgentHost, run_id: &str, after: u64) -> Result<Value, String> {
    let run = host.run(run_id).map_err(|error| error.to_string())?;
    let oldest = run.events.first().map_or(0, |event| event.sequence);
    let newest = run.events.last().map_or(0, |event| event.sequence);
    let stale_cursor = after > 0 && oldest > 0 && after.saturating_add(1) < oldest;
    let events = if stale_cursor {
        Vec::new()
    } else {
        run.events
            .iter()
            .filter(|event| event.sequence > after)
            .map(event_json)
            .collect::<Vec<_>>()
    };
    Ok(json!({
        "run_id": run_id,
        "after": after,
        "oldest_sequence": oldest,
        "newest_sequence": newest,
        "stale_cursor": stale_cursor,
        "snapshot_required": stale_cursor,
        "events": events,
    }))
}

pub(crate) fn frame_bytes(
    host: &AgentHost,
    run_id: &str,
    artifact_id: &str,
) -> Result<Vec<u8>, String> {
    host.captured_frame_artifact(run_id, artifact_id)
        .map(|(bytes, _, _)| bytes)
        .map_err(|error| error.to_string())
}

fn session_json(session: &AgentSession) -> Value {
    json!({
        "id": session.id,
        "title": sanitize_text(&session.title),
        "messages": session.messages.iter().map(|message| json!({
            "role": serde_json::to_value(&message.role).unwrap_or(Value::Null),
            "text": sanitize_text(&message.text),
            "created_unix_ms": message.created_unix_ms,
        })).collect::<Vec<_>>(),
        "proposal": {
            "version": session.proposal.version,
            "goal": sanitize_text(&session.proposal.goal),
            "requirements": sanitize_strings(&session.proposal.requirements),
            "assumptions": sanitize_strings(&session.proposal.assumptions),
            "acceptance_criteria": sanitize_strings(&session.proposal.acceptance_criteria),
            "planned_project_changes": sanitize_strings(&session.proposal.planned_project_changes),
            "planned_code_changes": sanitize_strings(&session.proposal.planned_code_changes),
            "planned_assets": sanitize_strings(&session.proposal.planned_assets),
            "validation_plan": sanitize_strings(&session.proposal.validation_plan),
            "playtest_plan": sanitize_strings(&session.proposal.playtest_plan),
        },
        "shared_with_project": session.shared_with_project,
    })
}

fn run_json(run: &AgentRun) -> Value {
    let frames = run.events.iter().filter_map(|event| {
        let AgentEventEvidence::CapturedFrame { artifact_id, width, height } = event.evidence.as_ref()? else {
            return None;
        };
        Some(json!({
            "artifact_id": artifact_id,
            "width": width,
            "height": height,
            "step": event.sequence,
        }))
    }).collect::<Vec<_>>();
    json!({
        "id": run.id,
        "proposal_version": run.proposal_snapshot.version,
        "state": serde_json::to_value(run.state).unwrap_or(Value::Null),
        "provider": sanitize_text(&run.provider_label),
        "started_unix_ms": run.started_unix_ms,
        "finished_unix_ms": run.finished_unix_ms,
        "completion": run.completion.clone(),
        "validation_attempts": run.validation_attempts.clone(),
        "audit": run.audit.clone(),
        "frames": frames,
        "last_event_sequence": run.events.last().map_or(0, |event| event.sequence),
    })
}

fn event_json(event: &AgentEvent) -> Value {
    let message = if matches!(event.kind, AgentEventKind::ProviderOutput) {
        "Provider output is intentionally hidden from Remote AI Studio.".to_owned()
    } else {
        sanitize_text(&event.message)
    };
    json!({
        "sequence": event.sequence,
        "created_unix_ms": event.created_unix_ms,
        "kind": serde_json::to_value(event.kind).unwrap_or(Value::Null),
        "message": message,
        "validation": event.validation.clone(),
        "evidence": safe_evidence(event),
    })
}

fn safe_evidence(event: &AgentEvent) -> Value {
    match event.evidence.as_ref() {
        Some(AgentEventEvidence::Progress { step, detail }) => json!({
            "evidence": "progress",
            "step": sanitize_text(step),
            "detail": sanitize_text(detail),
        }),
        Some(AgentEventEvidence::ToolAction { tool, action, success }) => json!({
            "evidence": "tool_action",
            "tool": sanitize_text(tool),
            "action": sanitize_text(action),
            "success": success,
        }),
        Some(AgentEventEvidence::Playtest { launched, interactions_passed }) => json!({
            "evidence": "playtest",
            "launched": launched,
            "interactions_passed": interactions_passed,
        }),
        Some(AgentEventEvidence::CapturedFrame { artifact_id, width, height }) => json!({
            "evidence": "captured_frame",
            "artifact_id": artifact_id,
            "width": width,
            "height": height,
        }),
        Some(AgentEventEvidence::CompletionGate { gate, status }) => json!({
            "evidence": "completion_gate",
            "gate": gate,
            "status": status,
        }),
        None => Value::Null,
    }
}

fn latest_safe_event_message(run: &AgentRun) -> String {
    run.events
        .last()
        .map(|event| sanitize_text(&event.message))
        .unwrap_or_else(|| "The run is waiting for user input.".to_owned())
}

fn sanitize_strings(values: &[String]) -> Vec<String> {
    values.iter().map(|value| sanitize_text(value)).collect()
}

fn sanitize_text(text: &str) -> String {
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("gameengine_mcp_auth_token")
        || lowered.contains("authorization: bearer")
        || lowered.contains("tailscale_auth")
        || lowered.contains("api_key=")
        || lowered.contains("secret=")
    {
        return "[sensitive detail redacted]".to_owned();
    }
    text.split_whitespace()
        .map(|token| {
            let path_like = token.starts_with("/home/")
                || token.starts_with("/Users/")
                || token.starts_with("\\\\")
                || token.as_bytes().get(1) == Some(&b':')
                    && token.as_bytes().get(2).is_some_and(|byte| matches!(byte, b'\\' | b'/'));
            if path_like {
                "[private-path]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_terminal(state: AgentRunState) -> bool {
    matches!(
        state,
        AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled
    )
}

const COMPANION_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>Remote AI Studio</title>
<style>
:root{font-family:system-ui,-apple-system,sans-serif;color-scheme:dark;background:#11151c;color:#eef2f7}*{box-sizing:border-box}body{margin:0}.shell{max-width:980px;margin:auto;padding:clamp(12px,3vw,28px);display:grid;gap:14px}.card{background:#1a202a;border:1px solid #303948;border-radius:14px;padding:14px;min-width:0}.row{display:flex;gap:8px;align-items:center;flex-wrap:wrap}.grow{flex:1;min-width:0}button,select,textarea,input{font:inherit;color:inherit;background:#11151c;border:1px solid #455166;border-radius:9px;padding:9px}button{cursor:pointer}button.primary{background:#285ea8}button.danger{background:#853c42}textarea{width:100%;min-height:76px;resize:vertical}.muted{color:#aab5c5;font-size:.9rem}.messages,.events{display:grid;gap:8px;max-height:300px;overflow:auto}.msg,.event{padding:9px;border-radius:9px;background:#11151c;overflow-wrap:anywhere}.proposal-grid,.completion{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:8px}.pill{display:inline-block;padding:3px 7px;border-radius:999px;background:#303948;font-size:.8rem}.frame{max-width:100%;height:auto;border-radius:10px;border:1px solid #455166}.error{color:#ffb3b3;overflow-wrap:anywhere}h1,h2,h3,p{margin-top:0}@media(max-width:640px){.shell{padding:10px}.proposal-grid,.completion{grid-template-columns:1fr}.row>button{flex:1}.card{padding:12px}.messages,.events{max-height:240px}h1{font-size:1.45rem}}
</style>
</head>
<body><main class="shell">
<section class="card"><div class="row"><div class="grow"><h1>Remote AI Studio</h1><div class="muted">Companion view over the Editor Agent Host · not a remote Editor</div></div><select id="sessions"></select></div><div id="error" class="error"></div></section>
<section class="card"><h2>Conversation</h2><div id="messages" class="messages"></div><textarea id="message" placeholder="Add a constraint or continue the conversation"></textarea><div class="row"><button id="send">Send</button></div></section>
<section class="card"><div class="row"><h2 class="grow">Proposal</h2><span id="proposalVersion" class="pill"></span></div><div id="proposal" class="proposal-grid"></div><div class="row"><button id="go" class="primary">Go</button><button id="stop" class="danger">Stop</button></div></section>
<section id="decisionCard" class="card" hidden><h2>Decision required</h2><div id="decision"></div></section>
<section class="card"><div class="row"><h2 class="grow">Run progress</h2><span id="runState" class="pill">idle</span></div><div id="events" class="events"></div><h3>Completion</h3><div id="completion" class="completion"></div></section>
<section class="card"><div class="row"><h2 class="grow">Live Game View</h2><span id="liveState" class="pill">stopped</span></div><div class="row"><label for="liveFps">Max FPS</label><select id="liveFps"><option>2</option><option selected>4</option><option>8</option></select><button id="liveStart" class="primary">Start live view</button><button id="liveStop" disabled>Stop live view</button></div><div id="liveMeta" class="muted">Game View only · authenticated transient PNG samples · latest frame retained only.</div><img id="liveFrame" class="frame" hidden alt="Live Game View observation"></section>
<section class="card"><h2>Captured frame</h2><div id="frameMeta" class="muted">No captured frame.</div><img id="frame" class="frame" hidden alt="Captured managed Play frame"></section>
</main>
<script>
const token=new URLSearchParams(location.hash.slice(1)).get('access_token')||''; history.replaceState(null,'',location.pathname); const h={'Authorization':'Bearer '+token}; let sessionId=null,snapshot=null,cursor=0,frameUrl=null,live=null,liveUrl=null,liveTimer=null;
const $=id=>document.getElementById(id); const rid=()=>crypto.randomUUID();
async function api(path,opt={}){const r=await fetch(path,{...opt,headers:{...h,...(opt.headers||{})}}); if(!r.ok){let e;try{e=await r.json()}catch{e={error:{message:'Request failed'}}}throw new Error(e.error?.message||'Request failed')}return r}
function text(v){return String(v??'')}
function list(items){return (items||[]).map(v=>'<div>'+escapeHtml(v)+'</div>').join('')||'<div class="muted">None</div>'}
function escapeHtml(v){return text(v).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]))}
async function loadSessions(){const data=await (await api('/api/sessions')).json(); const sel=$('sessions'); sel.innerHTML=''; for(const s of data.sessions){const o=document.createElement('option');o.value=s.id;o.textContent=s.title||s.id;sel.appendChild(o)} if(!sessionId&&data.sessions[0])sessionId=data.sessions[0].id; sel.value=sessionId||''; sel.onchange=()=>{sessionId=sel.value;cursor=0;refresh()}}
function renderSnapshot(s){snapshot=s; const m=$('messages');m.innerHTML=(s.session.messages||[]).map(x=>`<div class="msg"><b>${escapeHtml(x.role)}</b><div>${escapeHtml(x.text)}</div></div>`).join('')||'<div class="muted">No messages yet.</div>'; $('proposalVersion').textContent='v'+s.session.proposal.version; const p=s.session.proposal; $('proposal').innerHTML=`<div><b>Goal</b><div>${escapeHtml(p.goal)}</div></div><div><b>Acceptance criteria</b>${list(p.acceptance_criteria)}</div><div><b>Requirements</b>${list(p.requirements)}</div><div><b>Validation</b>${list(p.validation_plan)}</div><div><b>Playtest</b>${list(p.playtest_plan)}</div>`; const run=s.active_run; $('runState').textContent=run?.state||'idle'; $('stop').disabled=!run; const c=$('completion'); c.innerHTML=run?Object.entries(run.completion||{}).map(([k,v])=>`<div><b>${escapeHtml(k.replaceAll('_',' '))}</b><div>${escapeHtml(v)}</div></div>`).join(''):'<div class="muted">No active run.</div>'; renderDecision(s,run); renderFrame(run)}
function renderDecision(s,run){const card=$('decisionCard'),d=$('decision'); if(s.pending_permission){const p=s.pending_permission;card.hidden=false;d.innerHTML=`<p>${escapeHtml(p.label)}</p><div class="row">${['once','run','project','deny'].map(scope=>`<button data-scope="${scope}">${scope}</button>`).join('')}</div>`;d.querySelectorAll('button').forEach(b=>b.onclick=()=>permission(p.run_id,p.capability,b.dataset.scope));return} if(s.awaiting_user&&run){card.hidden=false;d.innerHTML='<textarea id="awaitText" placeholder="Response to the agent"></textarea><div class="row"><button id="awaitSend">Respond</button></div>';$('awaitSend').onclick=()=>awaiting(run.id,$('awaitText').value);return} card.hidden=true;d.innerHTML=''}
async function renderFrame(run){if(frameUrl){URL.revokeObjectURL(frameUrl);frameUrl=null} const img=$('frame'),meta=$('frameMeta'); const f=run?.frames?.at(-1); if(!f){img.hidden=true;meta.textContent='No captured frame.';return} meta.textContent=`${f.artifact_id} · ${f.width}×${f.height} · run ${run.id}`; try{const r=await api(`/api/runs/${encodeURIComponent(run.id)}/frames/${encodeURIComponent(f.artifact_id)}`);frameUrl=URL.createObjectURL(await r.blob());img.src=frameUrl;img.hidden=false}catch(e){img.hidden=true;showError(e)}}
async function refresh(){if(!sessionId)return; try{const s=await (await api('/api/sessions/'+encodeURIComponent(sessionId))).json();renderSnapshot(s);const run=s.active_run;if(live&&live.run_id!==run?.id)resetLiveView();if(run){const raw=await (await api(`/api/runs/${encodeURIComponent(run.id)}/events?after=${cursor}`)).text();const line=raw.split('\n').find(x=>x.startsWith('data: '));if(line){const batch=JSON.parse(line.slice(6));if(batch.stale_cursor){cursor=0}else{for(const e of batch.events)cursor=Math.max(cursor,e.sequence);$('events').innerHTML=batch.events.map(e=>`<div class="event"><span class="pill">#${e.sequence} ${escapeHtml(e.kind)}</span><div>${escapeHtml(e.message)}</div></div>`).join('')||$('events').innerHTML}}}}catch(e){showError(e)}}
async function send(){const v=$('message').value.trim();if(!v)return;await api(`/api/sessions/${encodeURIComponent(sessionId)}/messages`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({request_id:rid(),text:v})});$('message').value='';await refresh()}
async function go(){if(!snapshot)return;await api(`/api/sessions/${encodeURIComponent(sessionId)}/go`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({request_id:rid(),proposal_version:snapshot.session.proposal.version})});cursor=0;await refresh()}
async function stop(){const run=snapshot?.active_run;if(!run)return;await api(`/api/runs/${encodeURIComponent(run.id)}/stop`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({request_id:rid()})});await refresh()}
async function permission(run,capability,scope){await api(`/api/runs/${encodeURIComponent(run)}/permissions`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({request_id:rid(),capability,scope})});await refresh()}
async function awaiting(run,textValue){const v=textValue.trim();if(!v)return;await api(`/api/runs/${encodeURIComponent(run)}/awaiting-user`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({request_id:rid(),text:v})});await refresh()}
function resetLiveView(){if(liveTimer){clearTimeout(liveTimer);liveTimer=null}if(liveUrl){URL.revokeObjectURL(liveUrl);liveUrl=null}live=null;$('liveState').textContent='stopped';$('liveStart').disabled=false;$('liveStop').disabled=true;$('liveFrame').hidden=true;$('liveMeta').textContent='Game View only · authenticated transient PNG samples · latest frame retained only.'}
async function startLive(){const run=snapshot?.active_run;if(!run)throw new Error('Start an AgentRun before live observation.');if(live)await stopLive();const max_fps=Number($('liveFps').value);const data=await (await api(`/api/runs/${encodeURIComponent(run.id)}/live`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({request_id:rid(),max_fps})})).json();live={...data,sequence:0};$('liveState').textContent='live';$('liveStart').disabled=true;$('liveStop').disabled=false;pollLive()}
async function pollLive(){if(!live)return;const current=live;const mediaHeaders={'X-GameEngine-Media-Token':current.media_token};try{const status=await (await api(`/api/live/${encodeURIComponent(current.media_session_id)}`,{headers:mediaHeaders})).json();if(live!==current)return;$('liveState').textContent=status.last_error?'retrying':'live';$('liveMeta').textContent=`${status.source} · ${status.latest_width??'waiting'}×${status.latest_height??'waiting'} · ${status.max_fps} fps cap · samples ${status.capture_count} · readback ${status.latest_readback_micros??'—'} µs · encode ${status.latest_encode_micros??'—'} µs · E2E ${status.latest_end_to_end_micros??'—'} µs`;if(status.latest_sequence!=null&&status.latest_sequence!==current.sequence){const response=await api(`/api/live/${encodeURIComponent(current.media_session_id)}/frames/${status.latest_sequence}`,{headers:mediaHeaders});if(live!==current)return;if(liveUrl)URL.revokeObjectURL(liveUrl);liveUrl=URL.createObjectURL(await response.blob());$('liveFrame').src=liveUrl;$('liveFrame').hidden=false;current.sequence=status.latest_sequence}}catch(e){if(live===current){$('liveState').textContent='retrying';$('liveMeta').textContent=e.message||text(e)}}finally{if(live===current)liveTimer=setTimeout(pollLive,Math.max(125,Math.floor(1000/current.max_fps)))}}
async function stopLive(){if(!live){resetLiveView();return}const current=live;if(liveTimer){clearTimeout(liveTimer);liveTimer=null}try{await api(`/api/live/${encodeURIComponent(current.media_session_id)}/stop`,{method:'POST',headers:{'Content-Type':'application/json','X-GameEngine-Media-Token':current.media_token},body:JSON.stringify({request_id:rid()})})}finally{resetLiveView()}}
function showError(e){$('error').textContent=e.message||text(e)}
$('send').onclick=()=>send().catch(showError);$('go').onclick=()=>go().catch(showError);$('stop').onclick=()=>stop().catch(showError);$('liveStart').onclick=()=>startLive().catch(showError);$('liveStop').onclick=()=>stopLive().catch(showError);(async()=>{try{await loadSessions();await refresh();setInterval(refresh,1200)}catch(e){showError(e)}})();
</script></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_host::AgentEventKind;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gameengine-remote-ai-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    fn test_host(label: &str) -> (AgentHost, PathBuf, PathBuf) {
        let project = temp_path(&format!("{label}-project"));
        let storage = temp_path(&format!("{label}-storage"));
        fs::create_dir_all(&project).expect("project");
        (
            AgentHost::open(project.clone(), storage.clone()).expect("host"),
            project,
            storage,
        )
    }

    #[test]
    fn same_request_identity_executes_mutation_once() {
        let mut cache = MutationCache::default();
        let count = AtomicUsize::new(0);
        let operation = RemoteOperation::Stop {
            run_id: "run_1".into(),
            request_id: "mobile-1".into(),
        };
        let first = cache.execute(&operation, || {
            count.fetch_add(1, AtomicOrdering::Relaxed);
            RemoteAiStudioResponse::json(json!({"ok": true}))
        });
        let second = cache.execute(&operation, || {
            count.fetch_add(1, AtomicOrdering::Relaxed);
            RemoteAiStudioResponse::json(json!({"ok": false}))
        });
        assert_eq!(count.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(first.body, second.body);
    }

    #[test]
    fn duplicate_go_request_creates_one_agent_host_run() {
        let (mut host, project, storage) = test_host("go");
        let session = host.create_session("Remote").expect("session");
        let version = host.session(&session).expect("session").proposal.version;
        let mut cache = MutationCache::default();
        let operation = RemoteOperation::Go {
            session_id: session.clone(),
            request_id: "go-one".into(),
            proposal_version: version,
        };
        for _ in 0..2 {
            let _ = cache.execute(&operation, || {
                match host.start_run_authorized(&session, version, "test") {
                    Ok(run_id) => RemoteAiStudioResponse::json(json!({"run_id": run_id})),
                    Err(error) => RemoteAiStudioResponse::error(409, "go_failed", error.to_string(), false),
                }
            });
        }
        assert_eq!(host.session(&session).expect("session").runs.len(), 1);
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn duplicate_permission_and_awaiting_user_responses_are_applied_once() {
        let mut cache = MutationCache::default();
        let permission_count = AtomicUsize::new(0);
        let permission = RemoteOperation::Permission {
            run_id: "run_1".into(),
            request_id: "permission-one".into(),
            capability: AgentCapability::FrameCapture,
            scope: RemotePermissionScope::Once,
        };
        for _ in 0..2 {
            let _ = cache.execute(&permission, || {
                permission_count.fetch_add(1, AtomicOrdering::Relaxed);
                RemoteAiStudioResponse::json(json!({"ok": true}))
            });
        }
        let awaiting_count = AtomicUsize::new(0);
        let awaiting = RemoteOperation::AwaitingUser {
            run_id: "run_1".into(),
            request_id: "await-one".into(),
            text: "continue".into(),
        };
        for _ in 0..2 {
            let _ = cache.execute(&awaiting, || {
                awaiting_count.fetch_add(1, AtomicOrdering::Relaxed);
                RemoteAiStudioResponse::json(json!({"ok": true}))
            });
        }
        assert_eq!(permission_count.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(awaiting_count.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn stale_proposal_go_is_rejected_by_authoritative_host() {
        let (mut host, project, storage) = test_host("stale-go");
        let session = host.create_session("Remote").expect("session");
        let old = host.session(&session).expect("session").proposal.version;
        let proposal = host.session(&session).expect("session").proposal.clone();
        host.update_proposal(&session, proposal).expect("proposal");
        assert!(host.start_run_authorized(&session, old, "test").is_err());
        assert!(host.session(&session).expect("session").runs.is_empty());
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn reconnect_snapshot_restores_run_and_pending_decision_view() {
        let (mut host, project, storage) = test_host("reconnect");
        let session = host.create_session("Remote").expect("session");
        let version = host.session(&session).expect("session").proposal.version;
        let run = host.start_run_authorized(&session, version, "test").expect("run");
        host.transition_run(&run, AgentRunState::AwaitingUser, "Need input")
            .expect("awaiting");
        drop(host);
        let reopened = AgentHost::open(project.clone(), storage.clone()).expect("reopened");
        let snapshot = snapshot_json(&reopened, "project-a", &session, None).expect("snapshot");
        assert_eq!(snapshot["active_run"]["id"], run);
        assert_eq!(snapshot["awaiting_user"]["run_id"], run);
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn ordered_event_cursor_resumes_and_reports_stale_cursor() {
        let (mut host, project, storage) = test_host("events");
        let session = host.create_session("Remote").expect("session");
        let version = host.session(&session).expect("session").proposal.version;
        let run = host.start_run_authorized(&session, version, "test").expect("run");
        for index in 0..520 {
            host.record_event(&run, AgentEventKind::SemanticProgress, format!("event {index}"))
                .expect("event");
        }
        let batch = events_json(&host, &run, 515).expect("batch");
        let sequences = batch["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|event| event["sequence"].as_u64().expect("sequence"))
            .collect::<Vec<_>>();
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
        let stale = events_json(&host, &run, 1).expect("stale");
        assert_eq!(stale["stale_cursor"], true);
        assert_eq!(stale["snapshot_required"], true);
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn disconnect_does_not_cancel_active_run() {
        let (mut host, project, storage) = test_host("disconnect");
        let session = host.create_session("Remote").expect("session");
        let version = host.session(&session).expect("session").proposal.version;
        let run = host.start_run_authorized(&session, version, "test").expect("run");
        drop(MutationCache::default());
        assert!(!is_terminal(host.run(&run).expect("run").state));
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn mcp_credentials_and_provider_output_are_not_exposed() {
        let (mut host, project, storage) = test_host("sanitize");
        let session = host.create_session("Remote").expect("session");
        let version = host.session(&session).expect("session").proposal.version;
        let run = host.start_run_authorized(&session, version, "test").expect("run");
        host.record_event(
            &run,
            AgentEventKind::ProviderOutput,
            "GAMEENGINE_MCP_AUTH_TOKEN=super-secret C:\\Users\\private\\file",
        )
        .expect("provider output");
        let events = events_json(&host, &run, 0).expect("events").to_string();
        assert!(!events.contains("super-secret"));
        assert!(!events.contains("C:\\\\Users"));
        let snapshot = snapshot_json(&host, "project-a", &session, None)
            .expect("snapshot")
            .to_string();
        assert!(!snapshot.contains("GAMEENGINE_MCP_AUTH_TOKEN"));
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn unsafe_generic_remote_endpoints_do_not_exist() {
        for path in [
            "/api/shell",
            "/api/filesystem",
            "/api/git",
            "/api/mcp",
            "/api/process",
            "/api/desktop",
            "/api/desktop-capture",
        ] {
            let request = HttpRequest {
                method: "POST".into(),
                path: path.into(),
                headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
                body: b"{}".to_vec(),
            };
            assert!(route_request(&request).is_err(), "{path} must not be routed");
        }
    }

    #[test]
    fn captured_frame_is_scoped_to_project_session_and_run() {
        let (mut host, project, storage) = test_host("frame");
        let first_session = host.create_session("First").expect("session");
        let version = host.session(&first_session).expect("session").proposal.version;
        let first_run = host.start_run_authorized(&first_session, version, "test").expect("run");
        host.transition_run(&first_run, AgentRunState::Executing, "execute").expect("execute");
        host.transition_run(&first_run, AgentRunState::Validating, "validate").expect("validate");
        host.transition_run(&first_run, AgentRunState::Playtesting, "playtest").expect("playtest");
        let (artifact, _) = host.store_captured_frame_artifact(&first_run, 1, 1, b"png-one").expect("frame");
        let snapshot = snapshot_json(&host, "project-a", &first_session, None).expect("snapshot");
        assert_eq!(snapshot["project_id"], "project-a");
        assert_eq!(snapshot["session"]["id"], first_session);
        assert_eq!(snapshot["active_run"]["id"], first_run);
        assert_eq!(frame_bytes(&host, &first_run, &artifact).expect("bytes"), b"png-one");
        assert!(frame_bytes(&host, "run_wrong", &artifact).is_err());
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn live_media_routes_keep_source_and_media_auth_explicit() {
        let start = HttpRequest {
            method: "POST".into(),
            path: "/api/runs/run_1/live".into(),
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            body: br#"{"request_id":"live-1","max_fps":8}"#.to_vec(),
        };
        assert!(matches!(
            route_request(&start).expect("start route"),
            RemoteOperation::StartLiveObservation { run_id, max_fps: 8, .. } if run_id == "run_1"
        ));

        let frame = HttpRequest {
            method: "GET".into(),
            path: "/api/live/media_1/frames/7".into(),
            headers: BTreeMap::from([(
                "x-gameengine-media-token".into(),
                "media-secret".into(),
            )]),
            body: Vec::new(),
        };
        assert!(matches!(
            route_request(&frame).expect("frame route"),
            RemoteOperation::LiveObservationFrame {
                media_session_id,
                media_token,
                sequence: 7,
            } if media_session_id == "media_1" && media_token == "media-secret"
        ));
        assert!(COMPANION_HTML.contains("Live Game View"));
        assert!(!COMPANION_HTML.contains("desktop capture"));
    }

    #[test]
    fn gateway_binds_only_to_loopback() {
        let (server, _requests) = RemoteAiStudioServer::start(egui::Context::default()).expect("server");
        assert!(server.local_addr.ip().is_loopback());
    }
}
