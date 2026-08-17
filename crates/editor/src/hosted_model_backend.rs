//! Hosted and enterprise inference adapters for the native AI Studio harness.
//!
//! Provider credentials remain machine-local application secrets. On Windows,
//! GameEngine protects API keys with the current-user Data Protection API before
//! writing ciphertext below the Editor application-data root. The hosted request
//! path decrypts only for the lifetime of one request and never serializes the
//! credential into project or Agent Host state.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const MAX_PROVIDER_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SECRET_HELPER_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_PROVIDER_MESSAGE_CHARS: usize = 500;

const DPAPI_PROTECT_SCRIPT: &str = r#"$ErrorActionPreference='Stop';$plain=[Console]::In.ReadToEnd();$bytes=[Text.Encoding]::UTF8.GetBytes($plain);$protected=[Security.Cryptography.ProtectedData]::Protect($bytes,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser);[Console]::Out.Write([Convert]::ToBase64String($protected));if($bytes.Length -gt 0){[Array]::Clear($bytes,0,$bytes.Length)}"#;
const DPAPI_UNPROTECT_SCRIPT: &str = r#"$ErrorActionPreference='Stop';$cipher=[Console]::In.ReadToEnd().Trim();$protected=[Convert]::FromBase64String($cipher);$bytes=[Security.Cryptography.ProtectedData]::Unprotect($protected,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser);[Console]::Out.Write([Text.Encoding]::UTF8.GetString($bytes));if($bytes.Length -gt 0){[Array]::Clear($bytes,0,$bytes.Length)}"#;
const HOSTED_HTTP_SCRIPT: &str = r#"$ErrorActionPreference='Stop';$request=[Console]::In.ReadToEnd()|ConvertFrom-Json;$headers=@{};if($null -ne $request.authorization -and $request.authorization.Length -gt 0){$headers['Authorization']='Bearer '+$request.authorization};$parameters=@{Uri=$request.endpoint;Method='Post';Headers=$headers;ContentType='application/json';Body=$request.body;TimeoutSec=120;UseBasicParsing=$true};if($request.enterprise_managed){$parameters['UseDefaultCredentials']=$true};try{$response=Invoke-WebRequest @parameters;$result=[ordered]@{ok=$true;status=[int]$response.StatusCode;body=[string]$response.Content;message=''}}catch{$status=0;$body='';if($null -ne $_.Exception.Response){try{$status=[int]$_.Exception.Response.StatusCode.value__}catch{};try{$stream=$_.Exception.Response.GetResponseStream();if($null -ne $stream){$reader=New-Object IO.StreamReader($stream);$body=$reader.ReadToEnd();$reader.Dispose()}}catch{}};if($body.Length -eq 0 -and $null -ne $_.ErrorDetails){$body=[string]$_.ErrorDetails.Message};$result=[ordered]@{ok=$false;status=$status;body=$body;message=[string]$_.Exception.Message}};[Console]::Out.Write(($result|ConvertTo-Json -Compress -Depth 8))"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostedAuthMode {
    ApiKey,
    EnterpriseManaged,
}

impl HostedAuthMode {
    pub(crate) fn backend_id(self) -> &'static str {
        match self {
            Self::ApiKey => "openai-compatible-hosted",
            Self::EnterpriseManaged => "openai-compatible-enterprise",
        }
    }

    fn enterprise_managed(self) -> bool {
        self == Self::EnterpriseManaged
    }
}

#[derive(Debug, Clone)]
pub(crate) struct HostedModelConfig {
    pub(crate) endpoint: String,
    pub(crate) model: String,
    pub(crate) auth_mode: HostedAuthMode,
    pub(crate) encrypted_secret_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostedFailureCategory {
    UnsupportedPlatform,
    InvalidConfiguration,
    CredentialUnavailable,
    Authentication,
    RateLimited,
    SafetyRefusal,
    ContextTooLarge,
    ProviderRejected,
    Server,
    Transport,
    InvalidResponse,
    Interrupted,
}

#[derive(Debug)]
pub(crate) struct HostedBackendError {
    pub(crate) category: HostedFailureCategory,
    pub(crate) retryable: bool,
    pub(crate) status: Option<u16>,
    message: String,
}

impl HostedBackendError {
    fn new(category: HostedFailureCategory, retryable: bool, status: Option<u16>, message: impl Into<String>) -> Self {
        Self {
            category,
            retryable,
            status,
            message: sanitize_provider_message(&message.into()),
        }
    }

    pub(crate) fn interrupted() -> Self {
        Self::new(HostedFailureCategory::Interrupted, false, None, "hosted inference was interrupted")
    }
}

impl fmt::Display for HostedBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(formatter, "hosted model backend {:?} (HTTP {status}, retryable={}): {}", self.category, self.retryable, self.message),
            None => write!(formatter, "hosted model backend {:?} (retryable={}): {}", self.category, self.retryable, self.message),
        }
    }
}

impl std::error::Error for HostedBackendError {}

#[derive(Debug)]
pub(crate) struct HostedGeneration {
    pub(crate) text: String,
    pub(crate) prompt_tokens: Option<u64>,
    pub(crate) response_tokens: Option<u64>,
}

struct SecretBytes(Vec<u8>);

impl SecretBytes {
    fn expose(&self) -> Result<&str, HostedBackendError> {
        std::str::from_utf8(&self.0).map_err(|_| HostedBackendError::new(
            HostedFailureCategory::CredentialUnavailable,
            false,
            None,
            "stored credential is not valid UTF-8",
        ))
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

pub(crate) fn credential_is_configured(path: &Path) -> bool {
    path.is_file()
}

pub(crate) fn store_api_key(path: &Path, secret: &str) -> Result<(), HostedBackendError> {
    if secret.trim().is_empty() {
        return Err(HostedBackendError::new(
            HostedFailureCategory::CredentialUnavailable,
            false,
            None,
            "API credential cannot be empty",
        ));
    }
    ensure_windows()?;
    let encrypted = run_powershell(
        DPAPI_PROTECT_SCRIPT,
        secret.as_bytes(),
        &AtomicBool::new(false),
        MAX_SECRET_HELPER_OUTPUT_BYTES,
    )?;
    let parent = path.parent().ok_or_else(|| HostedBackendError::new(
        HostedFailureCategory::CredentialUnavailable,
        false,
        None,
        "credential path has no parent directory",
    ))?;
    fs::create_dir_all(parent).map_err(credential_io_error)?;
    let temp = path.with_extension("dpapi.tmp");
    fs::write(&temp, encrypted).map_err(credential_io_error)?;
    if path.exists() {
        fs::remove_file(path).map_err(credential_io_error)?;
    }
    fs::rename(&temp, path).map_err(credential_io_error)
}

pub(crate) fn remove_api_key(path: &Path) -> Result<(), HostedBackendError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(credential_io_error(error)),
    }
}

fn load_api_key(path: &Path) -> Result<SecretBytes, HostedBackendError> {
    ensure_windows()?;
    let ciphertext = fs::read(path).map_err(|error| {
        HostedBackendError::new(
            HostedFailureCategory::CredentialUnavailable,
            false,
            None,
            format!("could not read protected credential: {error}"),
        )
    })?;
    let plaintext = run_powershell(
        DPAPI_UNPROTECT_SCRIPT,
        &ciphertext,
        &AtomicBool::new(false),
        MAX_SECRET_HELPER_OUTPUT_BYTES,
    )?;
    if plaintext.is_empty() {
        return Err(HostedBackendError::new(
            HostedFailureCategory::CredentialUnavailable,
            false,
            None,
            "protected credential decrypted to an empty value",
        ));
    }
    Ok(SecretBytes(plaintext))
}

pub(crate) fn generate_hosted(
    config: &HostedModelConfig,
    prompt: &str,
    interrupted: &AtomicBool,
) -> Result<HostedGeneration, HostedBackendError> {
    ensure_windows()?;
    validate_https_endpoint(&config.endpoint)?;
    if config.model.trim().is_empty() {
        return Err(HostedBackendError::new(
            HostedFailureCategory::InvalidConfiguration,
            false,
            None,
            "a hosted model name is required",
        ));
    }
    if interrupted.load(Ordering::Acquire) {
        return Err(HostedBackendError::interrupted());
    }

    let secret = if config.auth_mode == HostedAuthMode::ApiKey {
        Some(load_api_key(&config.encrypted_secret_path)?)
    } else {
        None
    };
    let authorization = secret.as_ref().map(SecretBytes::expose).transpose()?;
    let request_body = serde_json::to_string(&ChatCompletionRequest {
        model: config.model.trim(),
        messages: [ChatMessage { role: "user", content: prompt }],
        stream: false,
    }).map_err(json_error)?;
    let envelope = HostedRequestEnvelope {
        endpoint: config.endpoint.trim(),
        authorization,
        enterprise_managed: config.auth_mode.enterprise_managed(),
        body: &request_body,
    };
    let mut input = serde_json::to_vec(&envelope).map_err(json_error)?;
    let output = run_powershell(
        HOSTED_HTTP_SCRIPT,
        &input,
        interrupted,
        MAX_PROVIDER_OUTPUT_BYTES,
    );
    input.fill(0);
    let output = output?;
    let transport: PowerShellHttpResult = serde_json::from_slice(&output).map_err(|_| HostedBackendError::new(
        HostedFailureCategory::InvalidResponse,
        false,
        None,
        "platform HTTP helper returned an invalid response envelope",
    ))?;
    if !transport.ok {
        return Err(classify_provider_failure(transport.status, &transport.body, &transport.message));
    }
    if transport.status < 200 || transport.status >= 300 {
        return Err(classify_provider_failure(transport.status, &transport.body, &transport.message));
    }
    parse_chat_completion(&transport.body)
}

fn validate_https_endpoint(endpoint: &str) -> Result<(), HostedBackendError> {
    let trimmed = endpoint.trim();
    let authority_and_path = trimmed.strip_prefix("https://").ok_or_else(|| HostedBackendError::new(
        HostedFailureCategory::InvalidConfiguration,
        false,
        None,
        "hosted model endpoints must use HTTPS",
    ))?;
    let authority = authority_and_path.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || trimmed.contains('#') {
        return Err(HostedBackendError::new(
            HostedFailureCategory::InvalidConfiguration,
            false,
            None,
            "hosted endpoint must contain an HTTPS host without embedded credentials or fragments",
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 1],
    stream: bool,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct HostedRequestEnvelope<'a> {
    endpoint: &'a str,
    authorization: Option<&'a str>,
    enterprise_managed: bool,
    body: &'a str,
}

#[derive(Deserialize)]
struct PowerShellHttpResult {
    ok: bool,
    status: u16,
    #[serde(default)]
    body: String,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    #[serde(default)]
    choices: Vec<ChatCompletionChoice>,
    #[serde(default)]
    usage: Option<ChatCompletionUsage>,
}

#[derive(Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatCompletionMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
}

#[derive(Deserialize)]
struct ChatCompletionUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

fn parse_chat_completion(body: &str) -> Result<HostedGeneration, HostedBackendError> {
    let response: ChatCompletionResponse = serde_json::from_str(body).map_err(|_| HostedBackendError::new(
        HostedFailureCategory::InvalidResponse,
        false,
        None,
        "hosted provider returned invalid chat-completion JSON",
    ))?;
    let choice = response.choices.first().ok_or_else(|| HostedBackendError::new(
        HostedFailureCategory::InvalidResponse,
        false,
        None,
        "hosted provider returned no completion choice",
    ))?;
    if choice.finish_reason.as_deref() == Some("content_filter")
        || choice.message.refusal.as_deref().is_some_and(|value| !value.trim().is_empty())
    {
        return Err(HostedBackendError::new(
            HostedFailureCategory::SafetyRefusal,
            false,
            None,
            "hosted provider refused the request under its safety policy",
        ));
    }
    let text = choice.message.content.as_deref().unwrap_or_default().trim().to_owned();
    if text.is_empty() {
        return Err(HostedBackendError::new(
            HostedFailureCategory::InvalidResponse,
            false,
            None,
            "hosted provider returned an empty completion",
        ));
    }
    Ok(HostedGeneration {
        text,
        prompt_tokens: response.usage.as_ref().and_then(|usage| usage.prompt_tokens),
        response_tokens: response.usage.as_ref().and_then(|usage| usage.completion_tokens),
    })
}

fn classify_provider_failure(status: u16, body: &str, fallback: &str) -> HostedBackendError {
    let message = if body.trim().is_empty() { fallback } else { body };
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("context length") || lowered.contains("maximum context") || lowered.contains("too many tokens") {
        return HostedBackendError::new(HostedFailureCategory::ContextTooLarge, false, status_option(status), "provider context limit was exceeded");
    }
    if lowered.contains("content_filter") || lowered.contains("safety policy") || lowered.contains("moderation") {
        return HostedBackendError::new(HostedFailureCategory::SafetyRefusal, false, status_option(status), "provider safety policy refused the request");
    }
    match status {
        0 => HostedBackendError::new(HostedFailureCategory::Transport, true, None, message),
        401 | 403 => HostedBackendError::new(HostedFailureCategory::Authentication, false, Some(status), "provider authentication or authorization failed"),
        408 | 425 => HostedBackendError::new(HostedFailureCategory::Transport, true, Some(status), message),
        429 => HostedBackendError::new(HostedFailureCategory::RateLimited, true, Some(status), "provider rate limit was reached"),
        500..=599 => HostedBackendError::new(HostedFailureCategory::Server, true, Some(status), message),
        _ => HostedBackendError::new(HostedFailureCategory::ProviderRejected, false, Some(status), message),
    }
}

fn status_option(status: u16) -> Option<u16> {
    (status != 0).then_some(status)
}

fn sanitize_provider_message(message: &str) -> String {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("authorization") || lowered.contains("bearer ") || lowered.contains("api_key") || lowered.contains("api key") || lowered.contains("secret") {
        return "provider returned a sensitive error; details were redacted".to_owned();
    }
    let trimmed = message.trim();
    if trimmed.chars().count() <= MAX_PROVIDER_MESSAGE_CHARS {
        trimmed.to_owned()
    } else {
        let mut output = trimmed.chars().take(MAX_PROVIDER_MESSAGE_CHARS).collect::<String>();
        output.push('…');
        output
    }
}

fn credential_io_error(error: io::Error) -> HostedBackendError {
    HostedBackendError::new(
        HostedFailureCategory::CredentialUnavailable,
        false,
        None,
        format!("secure credential storage failed: {error}"),
    )
}

fn json_error(error: serde_json::Error) -> HostedBackendError {
    HostedBackendError::new(HostedFailureCategory::InvalidConfiguration, false, None, format!("hosted request JSON failed: {error}"))
}

fn ensure_windows() -> Result<(), HostedBackendError> {
    if cfg!(target_os = "windows") {
        Ok(())
    } else {
        Err(HostedBackendError::new(
            HostedFailureCategory::UnsupportedPlatform,
            false,
            None,
            "first-release hosted credential protection requires Windows DPAPI; no insecure fallback is permitted",
        ))
    }
}

fn run_powershell(
    script: &str,
    input: &[u8],
    interrupted: &AtomicBool,
    max_output: usize,
) -> Result<Vec<u8>, HostedBackendError> {
    let mut child = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| HostedBackendError::new(HostedFailureCategory::Transport, true, None, format!("could not start Windows platform helper: {error}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input).map_err(|error| HostedBackendError::new(HostedFailureCategory::Transport, true, None, format!("could not send request to Windows platform helper: {error}")))?;
    }
    let stdout = child.stdout.take().ok_or_else(|| HostedBackendError::new(HostedFailureCategory::Transport, true, None, "Windows platform helper stdout was unavailable"))?;
    let reader = std::thread::spawn(move || read_capped(stdout, max_output));
    let status = loop {
        if interrupted.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(HostedBackendError::interrupted());
        }
        match child.try_wait().map_err(|error| HostedBackendError::new(HostedFailureCategory::Transport, true, None, format!("Windows platform helper wait failed: {error}")))? {
            Some(status) => break status,
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    };
    let output = reader.join().map_err(|_| HostedBackendError::new(HostedFailureCategory::Transport, true, None, "Windows platform helper output reader failed"))?
        .map_err(|error| HostedBackendError::new(HostedFailureCategory::Transport, true, None, format!("Windows platform helper output failed: {error}")))?;
    if !status.success() {
        return Err(HostedBackendError::new(HostedFailureCategory::Transport, true, None, "Windows platform helper failed"));
    }
    if output.len() > max_output {
        return Err(HostedBackendError::new(HostedFailureCategory::InvalidResponse, false, None, "hosted provider response exceeded the safety limit"));
    }
    Ok(output)
}

fn read_capped(mut reader: impl Read, max_output: usize) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if output.len() <= max_output {
            let remaining = max_output.saturating_add(1).saturating_sub(output.len());
            output.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_endpoint_requires_https_without_embedded_credentials() {
        assert!(validate_https_endpoint("https://provider.example/v1/chat/completions").is_ok());
        assert!(validate_https_endpoint("http://provider.example/v1/chat/completions").is_err());
        assert!(validate_https_endpoint("https://user:secret@provider.example/v1/chat/completions").is_err());
    }

    #[test]
    fn provider_failures_are_classified_without_success_fallback() {
        let auth = classify_provider_failure(401, "invalid token", "");
        assert_eq!(auth.category, HostedFailureCategory::Authentication);
        assert!(!auth.retryable);
        let rate = classify_provider_failure(429, "slow down", "");
        assert_eq!(rate.category, HostedFailureCategory::RateLimited);
        assert!(rate.retryable);
        let server = classify_provider_failure(503, "unavailable", "");
        assert_eq!(server.category, HostedFailureCategory::Server);
        assert!(server.retryable);
        let safety = classify_provider_failure(400, "content_filter policy", "");
        assert_eq!(safety.category, HostedFailureCategory::SafetyRefusal);
        assert!(!safety.retryable);
    }

    #[test]
    fn chat_completion_refusal_is_not_success() {
        let error = parse_chat_completion(r#"{"choices":[{"message":{"content":null,"refusal":"blocked"},"finish_reason":"content_filter"}]}"#)
            .expect_err("safety refusal must fail");
        assert_eq!(error.category, HostedFailureCategory::SafetyRefusal);
    }

    #[test]
    fn provider_message_redacts_auth_like_content() {
        assert_eq!(
            sanitize_provider_message("Authorization: Bearer super-secret"),
            "provider returned a sensitive error; details were redacted"
        );
    }
}
