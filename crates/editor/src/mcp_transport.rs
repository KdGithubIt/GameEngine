//! Loopback Streamable HTTP transport for the Editor-attached MCP endpoint.
//!
//! The transport owns HTTP/JSON-RPC framing and authentication only. Tool
//! business logic is dispatched back to the Editor thread, where the live
//! project authoring session remains authoritative.

use eframe::egui;
use engine_mcp::{McpToolDescriptor, authoring_tool_descriptors, tool_is_mutating};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub(crate) const MCP_PROTOCOL_VERSION: &str = "2026-07-28";
pub(crate) const LEGACY_MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_PATH: &str = "/mcp";
const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const HOST_POLL_INTERVAL: Duration = Duration::from_millis(100);
const REQUEST_PENDING: u8 = 0;
const REQUEST_EXECUTING: u8 = 1;
const REQUEST_CANCELLED: u8 = 2;

/// Authority carried by the credential that authenticated an MCP request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorMcpAccess {
    ReadWrite,
    AgentRunBound,
    ReadOnly,
}

/// Result returned by the live Editor host for one MCP tool request.
pub(crate) enum EditorMcpHostResult {
    Success(Value),
    ToolError { code: String, message: String },
}

/// One tool invocation waiting to run against the live Editor state.
pub(crate) struct EditorMcpRequest {
    name: String,
    arguments: Value,
    agent_run_id: Option<String>,
    lifecycle: Arc<AtomicU8>,
    response: mpsc::SyncSender<EditorMcpHostResult>,
}

impl EditorMcpRequest {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn arguments(&self) -> &Value {
        &self.arguments
    }

    pub(crate) fn agent_run_id(&self) -> Option<&str> {
        self.agent_run_id.as_deref()
    }

    pub(crate) fn try_begin(&self) -> bool {
        self.lifecycle
            .compare_exchange(
                REQUEST_PENDING,
                REQUEST_EXECUTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn respond(self, result: EditorMcpHostResult) {
        let _ = self.response.send(result);
    }
}

/// Failure to start the local Editor MCP transport.
#[derive(Debug)]
pub(crate) enum EditorMcpServerError {
    Bind(io::Error),
    Configure(io::Error),
    Random(String),
    Thread(io::Error),
}

impl fmt::Display for EditorMcpServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(error) => write!(
                formatter,
                "could not bind Editor MCP loopback endpoint: {error}"
            ),
            Self::Configure(error) => {
                write!(
                    formatter,
                    "could not configure Editor MCP loopback endpoint: {error}"
                )
            }
            Self::Random(error) => {
                write!(
                    formatter,
                    "could not create Editor MCP session credential: {error}"
                )
            }
            Self::Thread(error) => write!(
                formatter,
                "could not start Editor MCP listener thread: {error}"
            ),
        }
    }
}

impl std::error::Error for EditorMcpServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bind(error) | Self::Configure(error) | Self::Thread(error) => Some(error),
            Self::Random(_) => None,
        }
    }
}

/// Running loopback-only MCP server owned by one Editor process.
pub(crate) struct EditorMcpServer {
    local_addr: SocketAddr,
    endpoint: String,
    authorization_token: String,
    agent_authorization_token: String,
    read_only_authorization_token: String,
    shutdown: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<()>>,
}

impl EditorMcpServer {
    pub(crate) fn start(
        context: egui::Context,
    ) -> Result<(Self, mpsc::Receiver<EditorMcpRequest>), EditorMcpServerError> {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(EditorMcpServerError::Bind)?;
        listener
            .set_nonblocking(true)
            .map_err(EditorMcpServerError::Configure)?;
        let local_addr = listener
            .local_addr()
            .map_err(EditorMcpServerError::Configure)?;
        let endpoint = format!("http://{local_addr}{MCP_PATH}");
        let authorization_token = new_authorization_token()?;
        let agent_authorization_token = new_authorization_token()?;
        let read_only_authorization_token = new_authorization_token()?;
        let worker_token = authorization_token.clone();
        let worker_agent_token = agent_authorization_token.clone();
        let worker_read_only_token = read_only_authorization_token.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let (request_sender, request_receiver) = mpsc::channel();

        let listener_thread = thread::Builder::new()
            .name("gameengine-editor-mcp".into())
            .spawn(move || {
                serve_loop(
                    listener,
                    worker_token,
                    worker_agent_token,
                    worker_read_only_token,
                    request_sender,
                    context,
                    worker_shutdown,
                );
            })
            .map_err(EditorMcpServerError::Thread)?;

        Ok((
            Self {
                local_addr,
                endpoint,
                authorization_token,
                agent_authorization_token,
                read_only_authorization_token,
                shutdown,
                listener_thread: Some(listener_thread),
            },
            request_receiver,
        ))
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) fn authorization_token(&self) -> &str {
        &self.authorization_token
    }

    pub(crate) fn read_only_authorization_token(&self) -> &str {
        &self.read_only_authorization_token
    }

    pub(crate) fn agent_authorization_token(&self) -> &str {
        &self.agent_authorization_token
    }
}

impl Drop for EditorMcpServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.local_addr, Duration::from_millis(100));
        if let Some(thread) = self.listener_thread.take() {
            let _ = thread.join();
        }
    }
}

fn new_authorization_token() -> Result<String, EditorMcpServerError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| EditorMcpServerError::Random(error.to_string()))?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut token, "{byte:02x}").expect("writing hexadecimal text into String cannot fail");
    }
    Ok(token)
}

fn serve_loop(
    listener: TcpListener,
    authorization_token: String,
    agent_authorization_token: String,
    read_only_authorization_token: String,
    request_sender: mpsc::Sender<EditorMcpRequest>,
    context: egui::Context,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                let token = authorization_token.clone();
                let agent_token = agent_authorization_token.clone();
                let read_only_token = read_only_authorization_token.clone();
                let sender = request_sender.clone();
                let context = context.clone();
                let _ = thread::Builder::new()
                    .name("gameengine-editor-mcp-request".into())
                    .spawn(move || {
                        let _ = handle_connection(
                            stream,
                            &token,
                            &agent_token,
                            &read_only_token,
                            &sender,
                            &context,
                        );
                    });
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
    authorization_token: &str,
    agent_authorization_token: &str,
    read_only_authorization_token: &str,
    request_sender: &mpsc::Sender<EditorMcpRequest>,
    context: &egui::Context,
) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let request = match read_http_request(&mut stream) {
        Ok(request) => request,
        Err(HttpReadError::RequestTooLarge) => {
            return write_http_response(&mut stream, 413, "text/plain", b"request too large");
        }
        Err(HttpReadError::LengthRequired) => {
            return write_http_response(&mut stream, 411, "text/plain", b"content length required");
        }
        Err(HttpReadError::UnsupportedTransferEncoding) => {
            return write_http_response(
                &mut stream,
                501,
                "text/plain",
                b"transfer encoding is not supported",
            );
        }
        Err(HttpReadError::BadRequest) => {
            return write_http_response(&mut stream, 400, "text/plain", b"bad request");
        }
        Err(HttpReadError::Io(error)) => return Err(error),
    };

    let path = request
        .path
        .split('?')
        .next()
        .unwrap_or(request.path.as_str());
    if path != MCP_PATH {
        return write_http_response(&mut stream, 404, "text/plain", b"not found");
    }

    if request
        .headers
        .get("origin")
        .is_some_and(|origin| !origin_is_loopback(origin))
    {
        return write_http_response(&mut stream, 403, "text/plain", b"forbidden origin");
    }

    let authorization = request.headers.get("authorization");
    let access = if authorization == Some(&format!("Bearer {authorization_token}")) {
        EditorMcpAccess::ReadWrite
    } else if authorization == Some(&format!("Bearer {agent_authorization_token}")) {
        EditorMcpAccess::AgentRunBound
    } else if authorization == Some(&format!("Bearer {read_only_authorization_token}")) {
        EditorMcpAccess::ReadOnly
    } else {
        return write_http_response(&mut stream, 401, "text/plain", b"unauthorized");
    };

    match request.method.as_str() {
        "GET" => {
            return write_http_response(
                &mut stream,
                405,
                "text/plain",
                b"server-sent event stream is not enabled",
            );
        }
        "POST" => {}
        _ => {
            return write_http_response(&mut stream, 405, "text/plain", b"method not allowed");
        }
    }

    if !request
        .headers
        .get("content-type")
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"))
    {
        return write_http_response(
            &mut stream,
            415,
            "text/plain",
            b"application/json is required",
        );
    }

    let accepts = request
        .headers
        .get("accept")
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if !accepts.contains("application/json") || !accepts.contains("text/event-stream") {
        return write_http_response(
            &mut stream,
            406,
            "text/plain",
            b"accept must include application/json and text/event-stream",
        );
    }

    let message: Value = match serde_json::from_slice(&request.body) {
        Ok(Value::Object(message)) => Value::Object(message),
        Ok(_) => {
            return write_json_rpc_error(
                &mut stream,
                Value::Null,
                -32600,
                "invalid JSON-RPC request",
            );
        }
        Err(_) => {
            return write_json_rpc_error(&mut stream, Value::Null, -32700, "parse error");
        }
    };

    let object = message
        .as_object()
        .expect("JSON object was checked before dispatch");
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return write_json_rpc_error(
            &mut stream,
            object.get("id").cloned().unwrap_or(Value::Null),
            -32600,
            "jsonrpc must be 2.0",
        );
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return write_json_rpc_error(
            &mut stream,
            object.get("id").cloned().unwrap_or(Value::Null),
            -32600,
            "method is required",
        );
    };
    let id = object.get("id").cloned();

    let protocol_version = request
        .headers
        .get("mcp-protocol-version")
        .map(String::as_str);
    let modern = protocol_version == Some(MCP_PROTOCOL_VERSION) || method == "server/discover";
    if method != "initialize"
        && protocol_version != Some(MCP_PROTOCOL_VERSION)
        && protocol_version != Some(LEGACY_MCP_PROTOCOL_VERSION)
        && method != "server/discover"
    {
        return write_http_response(
            &mut stream,
            400,
            "text/plain",
            b"missing or unsupported MCP-Protocol-Version",
        );
    }
    if modern && request.headers.get("mcp-method").map(String::as_str) != Some(method) {
        return write_http_response(
            &mut stream,
            400,
            "text/plain",
            b"Mcp-Method must match the JSON-RPC method",
        );
    }
    if modern
        && object
            .get("params")
            .and_then(|params| params.get("_meta"))
            .and_then(|metadata| metadata.get("io.modelcontextprotocol/protocolVersion"))
            .and_then(Value::as_str)
            != Some(MCP_PROTOCOL_VERSION)
    {
        return write_http_response(
            &mut stream,
            400,
            "text/plain",
            b"modern MCP requests require the 2026-07-28 _meta protocol envelope",
        );
    }

    if id.is_none() {
        handle_notification(method);
        return write_http_response(&mut stream, 202, "text/plain", b"");
    }

    let id = id.unwrap_or(Value::Null);
    let result = match method {
        "initialize" => initialize_result(),
        "server/discover" => modern_discover_result(),
        "ping" => json!({}),
        "tools/list" => tools_list_result(access),
        "tools/call" => {
            let params = object.get("params").and_then(Value::as_object);
            let Some(name) = params
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
            else {
                return write_json_rpc_error(
                    &mut stream,
                    id,
                    -32602,
                    "tools/call requires a tool name",
                );
            };
            if modern && request.headers.get("mcp-name").map(String::as_str) != Some(name) {
                return write_http_response(
                    &mut stream,
                    400,
                    "text/plain",
                    b"Mcp-Name must match the requested tool",
                );
            }
            let arguments = params
                .and_then(|params| params.get("arguments"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            if access == EditorMcpAccess::ReadOnly && tool_is_mutating(name) {
                return write_json_rpc_error(
                    &mut stream,
                    id,
                    -32003,
                    "the read-only Editor credential cannot invoke mutating tools",
                );
            }
            let agent_run_id = request
                .headers
                .get("x-gameengine-agent-run-id")
                .cloned()
                .filter(|run_id| !run_id.trim().is_empty());
            if access == EditorMcpAccess::AgentRunBound && agent_run_id.is_none() {
                return write_json_rpc_error(
                    &mut stream,
                    id,
                    -32003,
                    "the external-agent MCP credential requires X-GameEngine-Agent-Run-Id",
                );
            }
            match dispatch_tool_call(
                name,
                arguments,
                agent_run_id,
                request_sender,
                context,
                &stream,
            ) {
                Ok(result) => result,
                Err(DispatchError::HostDisconnected) => {
                    return write_json_rpc_error(
                        &mut stream,
                        id,
                        -32603,
                        "Editor authoring host is unavailable",
                    );
                }
            }
        }
        _ => {
            return write_json_rpc_error(&mut stream, id, -32601, "method not found");
        }
    };

    let result = if modern {
        modern_result(result)
    } else {
        result
    };
    write_json_rpc_result(&mut stream, id, result)
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": LEGACY_MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": {
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": "gameengine-editor",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Structured authoring requests are routed to the live project-scoped Editor session."
    })
}

fn modern_discover_result() -> Value {
    modern_result(json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": false } },
        "instructions": "Structured authoring requests are routed to the live project-scoped Editor session."
    }))
}

fn modern_result(mut result: Value) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "resultType".to_owned(),
            Value::String("complete".to_owned()),
        );
        object.insert(
            "_meta".to_owned(),
            json!({
                "io.modelcontextprotocol/serverInfo": {
                    "name": "gameengine-editor",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        );
    }
    result
}

fn tools_list_result(access: EditorMcpAccess) -> Value {
    json!({
        "tools": authoring_tool_descriptors()
            .into_iter()
            .filter(|descriptor| {
                matches!(
                    access,
                    EditorMcpAccess::ReadWrite | EditorMcpAccess::AgentRunBound
                ) || !tool_is_mutating(&descriptor.name)
            })
            .map(tool_descriptor_json)
            .collect::<Vec<_>>()
    })
}

fn tool_descriptor_json(descriptor: McpToolDescriptor) -> Value {
    json!({
        "name": descriptor.name,
        "description": descriptor.description,
        "inputSchema": descriptor.input_schema
    })
}

fn handle_notification(_method: &str) {}

enum DispatchError {
    HostDisconnected,
}

fn dispatch_tool_call(
    name: &str,
    arguments: Value,
    agent_run_id: Option<String>,
    request_sender: &mpsc::Sender<EditorMcpRequest>,
    context: &egui::Context,
    stream: &TcpStream,
) -> Result<Value, DispatchError> {
    let (response_sender, response_receiver) = mpsc::sync_channel(1);
    let lifecycle = Arc::new(AtomicU8::new(REQUEST_PENDING));
    request_sender
        .send(EditorMcpRequest {
            name: name.to_owned(),
            arguments,
            agent_run_id,
            lifecycle: Arc::clone(&lifecycle),
            response: response_sender,
        })
        .map_err(|_| DispatchError::HostDisconnected)?;
    context.request_repaint();

    stream
        .set_read_timeout(Some(HOST_POLL_INTERVAL))
        .map_err(|_| DispatchError::HostDisconnected)?;
    let response = loop {
        match response_receiver.recv_timeout(HOST_POLL_INTERVAL) {
            Ok(response) => break response,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(DispatchError::HostDisconnected);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let mut probe = [0_u8; 1];
                match stream.peek(&mut probe) {
                    Ok(0) => {
                        let _ = lifecycle.compare_exchange(
                            REQUEST_PENDING,
                            REQUEST_CANCELLED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        );
                        return Err(DispatchError::HostDisconnected);
                    }
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) => {}
                    Err(_) => {
                        let _ = lifecycle.compare_exchange(
                            REQUEST_PENDING,
                            REQUEST_CANCELLED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        );
                        return Err(DispatchError::HostDisconnected);
                    }
                }
            }
        }
    };
    Ok(match response {
        EditorMcpHostResult::Success(value) => json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&value)
                    .unwrap_or_else(|_| "{\"error\":\"result serialization failed\"}".into())
            }],
            "structuredContent": value
        }),
        EditorMcpHostResult::ToolError { code, message } => json!({
            "content": [{
                "type": "text",
                "text": message.clone()
            }],
            "structuredContent": {
                "code": code,
                "message": message
            },
            "isError": true
        }),
    })
}

struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

enum HttpReadError {
    Io(io::Error),
    BadRequest,
    RequestTooLarge,
    LengthRequired,
    UnsupportedTransferEncoding,
}

impl From<io::Error> for HttpReadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, HttpReadError> {
    let mut buffer = Vec::with_capacity(4096);
    let header_end = loop {
        if buffer.len() > MAX_HEADER_BYTES {
            return Err(HttpReadError::RequestTooLarge);
        }
        if let Some(position) = find_bytes(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(HttpReadError::BadRequest);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let header_text =
        std::str::from_utf8(&buffer[..header_end - 4]).map_err(|_| HttpReadError::BadRequest)?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().ok_or(HttpReadError::BadRequest)?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or(HttpReadError::BadRequest)?
        .to_owned();
    let path = request_parts
        .next()
        .ok_or(HttpReadError::BadRequest)?
        .to_owned();
    if request_parts.next().is_none() || request_parts.next().is_some() {
        return Err(HttpReadError::BadRequest);
    }

    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(HttpReadError::BadRequest)?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }

    if headers.contains_key("transfer-encoding") {
        return Err(HttpReadError::UnsupportedTransferEncoding);
    }
    let content_length = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| HttpReadError::BadRequest)?,
        None if method == "POST" => return Err(HttpReadError::LengthRequired),
        None => 0,
    };
    if content_length > MAX_BODY_BYTES {
        return Err(HttpReadError::RequestTooLarge);
    }

    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let mut chunk = vec![0_u8; remaining.min(4096)];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(HttpReadError::BadRequest);
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn origin_is_loopback(origin: &str) -> bool {
    let authority = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"));
    let Some(authority) = authority else {
        return false;
    };
    if authority.contains('/') || authority.contains('@') {
        return false;
    }
    if authority == "localhost"
        || authority.starts_with("localhost:")
        || authority == "127.0.0.1"
        || authority.starts_with("127.0.0.1:")
        || authority == "[::1]"
        || authority.starts_with("[::1]:")
    {
        return true;
    }
    false
}

fn write_json_rpc_result(stream: &mut TcpStream, id: Value, result: Value) -> io::Result<()> {
    let body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    }))
    .expect("JSON-RPC result values are serializable");
    write_http_response(stream, 200, "application/json", &body)
}

fn write_json_rpc_error(
    stream: &mut TcpStream,
    id: Value,
    code: i64,
    message: &str,
) -> io::Result<()> {
    let body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    }))
    .expect("JSON-RPC error values are serializable");
    write_http_response(stream, 200, "application/json", &body)
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        501 => "Not Implemented",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    fn start() -> (EditorMcpServer, mpsc::Receiver<EditorMcpRequest>) {
        EditorMcpServer::start(egui::Context::default()).expect("MCP server must bind")
    }

    fn exchange(server: &EditorMcpServer, request: &str) -> String {
        let mut stream = TcpStream::connect(server.local_addr).expect("loopback connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        stream.write_all(request.as_bytes()).expect("request write");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("response read");
        response
    }

    fn post(server: &EditorMcpServer, body: &str, extra_headers: &str) -> String {
        post_with_token(server, server.authorization_token(), body, extra_headers)
    }

    fn post_with_token(
        server: &EditorMcpServer,
        token: &str,
        body: &str,
        extra_headers: &str,
    ) -> String {
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\n{}\r\n{}",
            token,
            body.len(),
            extra_headers,
            body
        );
        exchange(server, &request)
    }

    #[test]
    fn endpoint_is_loopback_only_and_requires_authorization() {
        let (server, _requests) = start();
        assert!(server.endpoint().starts_with("http://127.0.0.1:"));

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response = exchange(&server, &request);
        assert!(response.starts_with("HTTP/1.1 401"));
    }

    #[test]
    fn hostile_browser_origin_is_rejected_before_json_rpc_dispatch() {
        let (server, _requests) = start();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let response = post(&server, body, "Origin: https://example.invalid\r\n");
        assert!(response.starts_with("HTTP/1.1 403"));
    }

    #[test]
    fn initialize_and_tools_list_report_current_transport_surface() {
        let (server, _requests) = start();
        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#;
        let response = post(&server, initialize, "");
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("\"protocolVersion\":\"2025-11-25\""));

        let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let response = post(
            &server,
            list,
            "MCP-Protocol-Version: 2025-11-25\r\nOrigin: http://localhost:3000\r\n",
        );
        assert!(response.contains("\"scene.apply\""));
        assert!(response.contains("\"behavior_tree.apply\""));
    }

    #[test]
    fn tool_call_is_routed_to_live_host_channel() {
        let (server, requests) = start();
        let host = thread::spawn(move || {
            let request = requests
                .recv_timeout(Duration::from_secs(2))
                .expect("tool request must arrive");
            assert_eq!(request.name(), "project.describe");
            assert_eq!(request.arguments(), &json!({}));
            assert_eq!(request.agent_run_id(), Some("run-test"));
            request.respond(EditorMcpHostResult::Success(json!({"project":"ok"})));
        });

        let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"project.describe","arguments":{}}}"#;
        let response = post(
            &server,
            call,
            "MCP-Protocol-Version: 2025-11-25\r\nX-GameEngine-Agent-Run-Id: run-test\r\n",
        );
        host.join().expect("host thread");
        assert!(response.contains("\"structuredContent\":{\"project\":\"ok\"}"));
    }

    #[test]
    fn read_only_credential_lists_reads_and_rejects_mutations_before_dispatch() {
        let (server, requests) = start();
        let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let response = post_with_token(
            &server,
            server.read_only_authorization_token(),
            list,
            "MCP-Protocol-Version: 2025-11-25\r\n",
        );
        let body = response.split("\r\n\r\n").nth(1).expect("HTTP body");
        let json: Value = serde_json::from_str(body).expect("JSON-RPC response");
        let names = json["result"]["tools"]
            .as_array()
            .expect("tool list")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"project.describe"));
        assert!(!names.contains(&"scene.apply"));

        let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"scene.apply","arguments":{}}}"#;
        let response = post_with_token(
            &server,
            server.read_only_authorization_token(),
            call,
            "MCP-Protocol-Version: 2025-11-25\r\n",
        );
        assert!(response.contains("read-only Editor credential"));
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn agent_credential_requires_a_run_id_before_dispatch() {
        let (server, requests) = start();
        let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"project.describe","arguments":{}}}"#;
        let response = post_with_token(
            &server,
            server.agent_authorization_token(),
            call,
            "MCP-Protocol-Version: 2025-11-25\r\n",
        );
        assert!(response.contains("X-GameEngine-Agent-Run-Id"));
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn modern_discovery_requires_matching_method_header_and_stamps_results() {
        let (server, _requests) = start();
        let discover = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#;
        let response = post(
            &server,
            discover,
            "MCP-Protocol-Version: 2026-07-28\r\nMcp-Method: server/discover\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("\"resultType\":\"complete\""));
        assert!(response.contains("io.modelcontextprotocol/serverInfo"));
    }
}
