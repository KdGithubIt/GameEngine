//! Pollable ACP startup for AI Studio.
//!
//! Provider discovery, process launch, Managed Local endpoint startup, ACP
//! initialization, and session negotiation may block. They run on this worker
//! rather than on the egui presentation thread.

use crate::acp_agent_runtime::{
    AcpAgentRuntime, AcpAgentSession, AcpProcessRuntime, AcpSessionOpenRequest,
};
use crate::claude_acp_adapter::{ClaudeAcpConfig, discover_claude_acp};
use crate::codex_acp_adapter::{CodexAcpRuntime, CodexAcpSessionPreferences};
use crate::external_agent_provider::ExternalAgentExecutionPlacement;
use crate::goose_local_acp::{GooseLocalAcpConfig, GooseLocalAcpRuntime};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

pub(super) enum AcpRuntimeStartupConfig {
    Codex {
        placement: ExternalAgentExecutionPlacement,
        preferences: CodexAcpSessionPreferences,
    },
    Claude {
        placement: ExternalAgentExecutionPlacement,
        config: ClaudeAcpConfig,
    },
    Goose {
        config: Box<GooseLocalAcpConfig>,
    },
}

impl AcpRuntimeStartupConfig {
    pub(super) fn starting_status(&self) -> &'static str {
        match self {
            Self::Codex { .. } => "Starting Codex ACP…",
            Self::Claude { .. } => "Starting Claude ACP…",
            Self::Goose { .. } => "Starting Goose…",
        }
    }

    fn opening_status(&self) -> &'static str {
        match self {
            Self::Goose { .. } => "Starting Managed Local and connecting ACP…",
            Self::Codex { .. } | Self::Claude { .. } => "Connecting ACP…",
        }
    }

    fn discover(self) -> Result<Box<dyn AcpAgentRuntime>, String> {
        match self {
            Self::Codex {
                placement,
                preferences,
            } => CodexAcpRuntime::discover(placement, preferences)
                .map(|runtime| Box::new(runtime) as Box<dyn AcpAgentRuntime>)
                .map_err(|error| format!("Could not start Codex ACP: {error}")),
            Self::Claude { placement, config } => {
                let registration = discover_claude_acp(&config, &placement)
                    .map_err(|error| format!("Could not start Claude ACP: {error}"))?;
                AcpProcessRuntime::new(registration.descriptor)
                    .map(|runtime| Box::new(runtime) as Box<dyn AcpAgentRuntime>)
                    .map_err(|error| format!("Could not start Claude ACP: {error}"))
            }
            Self::Goose { config } => GooseLocalAcpRuntime::discover(*config)
                .map(|runtime| Box::new(runtime) as Box<dyn AcpAgentRuntime>)
                .map_err(|error| format!("Could not start Goose: {error}")),
        }
    }
}

pub(super) struct AcpSessionStartupResult {
    pub(super) runtime: Box<dyn AcpAgentRuntime>,
    pub(super) session: Box<dyn AcpAgentSession>,
}

pub(super) struct AcpSessionStartupTask {
    result: mpsc::Receiver<Result<AcpSessionStartupResult, String>>,
    progress: mpsc::Receiver<String>,
    cancelled: Arc<AtomicBool>,
    #[cfg(feature = "visual-validation")]
    _visual_result_hold: Option<mpsc::Sender<Result<AcpSessionStartupResult, String>>>,
    #[cfg(feature = "visual-validation")]
    _visual_progress_hold: Option<mpsc::Sender<String>>,
}

impl AcpSessionStartupTask {
    pub(super) fn spawn(
        config: AcpRuntimeStartupConfig,
        request: AcpSessionOpenRequest,
    ) -> Result<Self, String> {
        let opening_status = config.opening_status().to_owned();
        Self::spawn_with(request, opening_status, move || config.discover())
    }

    fn spawn_with<F>(
        request: AcpSessionOpenRequest,
        opening_status: String,
        discover: F,
    ) -> Result<Self, String>
    where
        F: FnOnce() -> Result<Box<dyn AcpAgentRuntime>, String> + Send + 'static,
    {
        let (result_tx, result) = mpsc::channel();
        let (progress_tx, progress) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        thread::Builder::new()
            .name("ai-studio-acp-startup".to_owned())
            .spawn(move || {
                if worker_cancelled.load(Ordering::Acquire) {
                    return;
                }
                let mut runtime = match discover() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = result_tx.send(Err(error));
                        return;
                    }
                };
                if worker_cancelled.load(Ordering::Acquire) {
                    return;
                }
                let _ = progress_tx.send(opening_status);
                let outcome = runtime
                    .open_session(request)
                    .map(|session| AcpSessionStartupResult { runtime, session })
                    .map_err(|error| format!("Could not open ACP session: {error}"));
                if worker_cancelled.load(Ordering::Acquire) {
                    if let Ok(mut started) = outcome {
                        let _ = started.session.cancel();
                        let _ = started.session.close();
                    }
                    return;
                }
                if let Err(send_error) = result_tx.send(outcome)
                    && let Ok(mut started) = send_error.0
                {
                    let _ = started.session.cancel();
                    let _ = started.session.close();
                }
            })
            .map_err(|error| format!("Could not start ACP startup worker: {error}"))?;
        Ok(Self {
            result,
            progress,
            cancelled,
            #[cfg(feature = "visual-validation")]
            _visual_result_hold: None,
            #[cfg(feature = "visual-validation")]
            _visual_progress_hold: None,
        })
    }

    #[cfg(feature = "visual-validation")]
    pub(super) fn visual_pending(progress_text: &str) -> Self {
        let (visual_result_hold, result) = mpsc::channel();
        let (visual_progress_hold, progress) = mpsc::channel();
        let _ = visual_progress_hold.send(progress_text.to_owned());
        Self {
            result,
            progress,
            cancelled: Arc::new(AtomicBool::new(false)),
            _visual_result_hold: Some(visual_result_hold),
            _visual_progress_hold: Some(visual_progress_hold),
        }
    }

    pub(super) fn poll(&self) -> Option<Result<AcpSessionStartupResult, String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) if self.cancelled.load(Ordering::Acquire) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(
                "ACP startup worker disconnected unexpectedly.".to_owned(),
            )),
        }
    }

    pub(super) fn latest_progress(&self) -> Option<String> {
        self.progress.try_iter().last()
    }

    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl Drop for AcpSessionStartupTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp_agent_runtime::{
        AcpAgentDescriptor, AcpCapabilities, AcpNormalizedEvent, AcpPermissionResolution,
        AcpRuntimeError, AcpRuntimeIdentity, AcpSessionBinding,
    };
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::time::Duration;

    struct TestSession {
        binding: AcpSessionBinding,
        cancel_observed: Option<mpsc::Sender<()>>,
        close_observed: Option<mpsc::Sender<()>>,
    }

    impl AcpAgentSession for TestSession {
        fn acp_session_id(&self) -> &str {
            "test-acp-session"
        }

        fn binding(&self) -> &AcpSessionBinding {
            &self.binding
        }

        fn capabilities(&self) -> &AcpCapabilities {
            static CAPABILITIES: std::sync::LazyLock<AcpCapabilities> =
                std::sync::LazyLock::new(AcpCapabilities::default);
            &CAPABILITIES
        }

        fn runtime_identity(&self) -> &AcpRuntimeIdentity {
            static IDENTITY: std::sync::LazyLock<AcpRuntimeIdentity> =
                std::sync::LazyLock::new(|| {
                    AcpRuntimeIdentity::stable("test-agent", Some("1.0".to_owned()))
                });
            &IDENTITY
        }

        fn send_prompt(&mut self, _prompt: &str) -> Result<(), AcpRuntimeError> {
            Ok(())
        }

        fn try_next_event(&mut self) -> Result<Option<AcpNormalizedEvent>, AcpRuntimeError> {
            Ok(None)
        }

        fn resolve_permission(
            &mut self,
            _resolution: AcpPermissionResolution,
        ) -> Result<(), AcpRuntimeError> {
            Ok(())
        }

        fn cancel(&mut self) -> Result<(), AcpRuntimeError> {
            if let Some(sender) = self.cancel_observed.take() {
                let _ = sender.send(());
            }
            Ok(())
        }

        fn close(&mut self) -> Result<(), AcpRuntimeError> {
            if let Some(sender) = self.close_observed.take() {
                let _ = sender.send(());
            }
            Ok(())
        }
    }

    struct BlockingRuntime {
        descriptor: AcpAgentDescriptor,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        cancel_observed: Option<mpsc::Sender<()>>,
        close_observed: Option<mpsc::Sender<()>>,
    }

    impl AcpAgentRuntime for BlockingRuntime {
        fn descriptor(&self) -> &AcpAgentDescriptor {
            &self.descriptor
        }

        fn open_session(
            &mut self,
            request: AcpSessionOpenRequest,
        ) -> Result<Box<dyn AcpAgentSession>, AcpRuntimeError> {
            self.entered
                .send(())
                .map_err(|_| AcpRuntimeError::Transport("test observer closed".to_owned()))?;
            self.release
                .recv()
                .map_err(|_| AcpRuntimeError::Transport("test release closed".to_owned()))?;
            Ok(Box::new(TestSession {
                binding: request.binding,
                cancel_observed: self.cancel_observed.take(),
                close_observed: self.close_observed.take(),
            }))
        }
    }

    fn descriptor() -> AcpAgentDescriptor {
        AcpAgentDescriptor {
            id: "test.acp".to_owned(),
            executable: OsString::from("test-acp"),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            capabilities: AcpCapabilities::default(),
            runtime_identity: AcpRuntimeIdentity::stable("test-agent", Some("1.0".to_owned())),
        }
    }

    fn request() -> AcpSessionOpenRequest {
        AcpSessionOpenRequest::new(
            AcpSessionBinding::read_only("session-test", "http://127.0.0.1:1/mcp", "token")
                .expect("binding"),
            std::env::current_dir().expect("current dir"),
        )
        .expect("request")
    }

    #[test]
    fn poll_returns_while_discovery_is_pending() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let task = AcpSessionStartupTask::spawn_with(
            request(),
            "Connecting ACP…".to_owned(),
            move || {
                entered_tx.send(()).expect("entered");
                release_rx.recv().expect("release");
                Err("controlled discovery failure".to_owned())
            },
        )
        .expect("spawn");
        entered_rx.recv().expect("worker entered");
        assert!(task.poll().is_none());
        release_tx.send(()).expect("release worker");
        let result = task
            .result
            .recv_timeout(Duration::from_secs(1))
            .expect("result");
        assert!(matches!(
            result,
            Err(ref error) if error == "controlled discovery failure"
        ));
    }

    #[test]
    fn poll_returns_while_session_negotiation_is_pending() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let runtime = BlockingRuntime {
            descriptor: descriptor(),
            entered: entered_tx,
            release: release_rx,
            cancel_observed: None,
            close_observed: None,
        };
        let task = AcpSessionStartupTask::spawn_with(
            request(),
            "Connecting ACP…".to_owned(),
            move || Ok(Box::new(runtime)),
        )
        .expect("spawn");
        entered_rx.recv().expect("worker entered open");
        assert!(task.poll().is_none());
        release_tx.send(()).expect("release open");
        let mut started = task
            .result
            .recv_timeout(Duration::from_secs(1))
            .expect("result")
            .expect("open");
        started.session.close().expect("close");
    }

    #[test]
    fn cancel_during_open_cleans_up_late_session() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        let (close_tx, close_rx) = mpsc::channel();
        let runtime = BlockingRuntime {
            descriptor: descriptor(),
            entered: entered_tx,
            release: release_rx,
            cancel_observed: Some(cancel_tx),
            close_observed: Some(close_tx),
        };
        let task = AcpSessionStartupTask::spawn_with(
            request(),
            "Connecting ACP…".to_owned(),
            move || Ok(Box::new(runtime)),
        )
        .expect("spawn");
        entered_rx.recv().expect("worker entered open");
        task.cancel();
        release_tx.send(()).expect("release open");
        cancel_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cancelled");
        close_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("closed");
        assert!(task.poll().is_none());
    }
}
