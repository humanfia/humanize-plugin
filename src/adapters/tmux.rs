use std::env;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adapters::lifecycle::{
    AdapterCapabilities, AgentLifecycleAdapter, LifecycleCleanup, LifecycleCleanupAction,
    LifecycleStatus,
};
use crate::pipe_sink::{
    PipeSinkCompletionPayload, PipeSinkIdentity, ensure_durable_pipe_capture_supported,
    pipe_sink_identity, remove_pipe_sink_ack_under_root, verify_pipe_sink_completion_under_root,
};

mod input_transaction;
mod pane_creation;
mod pane_presence;
mod pipe_capture;

pub use input_transaction::{TmuxInputTransaction, TmuxInputTransactionConfig};
pub(crate) use pane_presence::TmuxPanePresence;
pub(crate) use pipe_capture::PipeCaptureRequest;
use pipe_capture::{
    default_pipe_completion_path, helper_process_matches, pipe_ack_nonce, pipe_completion_error,
    pipe_sink_redacted_argv, redact_pipe_sink_error, shell_single_quote, shell_single_quote_str,
    wait_for_pipe_ack, wait_for_pipe_helper_exit,
};

pub(crate) fn new_pipe_capture_nonce() -> String {
    pipe_ack_nonce()
}

#[derive(Debug, Clone)]
pub struct TmuxAdapter<R: CommandRunner = SystemCommandRunner> {
    runner: R,
    input_config: TmuxInputTransactionConfig,
    pipe_sink_executable: Option<PathBuf>,
    pipe_ready_timeout: Duration,
    pipe_completion_timeout: Duration,
}

#[derive(Clone, Eq, PartialEq)]
pub struct TmuxPipeCapture {
    root: PathBuf,
    transcript_relative_path: PathBuf,
    completion_relative_path: PathBuf,
    transcript_identity: PipeSinkIdentity,
    nonce: String,
    helper_pid: u32,
    helper_process_start_time_ticks: u64,
    external_helper: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TmuxPipeCaptureDescriptor {
    pub transcript_relative_path: PathBuf,
    pub completion_relative_path: PathBuf,
    pub transcript_identity: PipeSinkIdentity,
    pub nonce: String,
    pub helper_pid: u32,
    pub helper_process_start_time_ticks: u64,
    pub external_helper: bool,
}

impl fmt::Debug for TmuxPipeCapture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TmuxPipeCapture")
            .field("root", &self.root)
            .field("transcript_relative_path", &self.transcript_relative_path)
            .field("completion_relative_path", &self.completion_relative_path)
            .field("transcript_identity", &self.transcript_identity)
            .field("helper_pid", &self.helper_pid)
            .field(
                "helper_process_start_time_ticks",
                &self.helper_process_start_time_ticks,
            )
            .field("external_helper", &self.external_helper)
            .finish()
    }
}

impl TmuxPipeCapture {
    pub fn descriptor(&self) -> TmuxPipeCaptureDescriptor {
        TmuxPipeCaptureDescriptor {
            transcript_relative_path: self.transcript_relative_path.clone(),
            completion_relative_path: self.completion_relative_path.clone(),
            transcript_identity: self.transcript_identity,
            nonce: self.nonce.clone(),
            helper_pid: self.helper_pid,
            helper_process_start_time_ticks: self.helper_process_start_time_ticks,
            external_helper: self.external_helper,
        }
    }

    pub fn from_descriptor(
        root: impl Into<PathBuf>,
        descriptor: TmuxPipeCaptureDescriptor,
    ) -> Self {
        Self {
            root: root.into(),
            transcript_relative_path: descriptor.transcript_relative_path,
            completion_relative_path: descriptor.completion_relative_path,
            transcript_identity: descriptor.transcript_identity,
            nonce: descriptor.nonce,
            helper_pid: descriptor.helper_pid,
            helper_process_start_time_ticks: descriptor.helper_process_start_time_ticks,
            external_helper: descriptor.external_helper,
        }
    }
}

impl TmuxAdapter<SystemCommandRunner> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for TmuxAdapter<SystemCommandRunner> {
    fn default() -> Self {
        Self {
            runner: SystemCommandRunner,
            input_config: TmuxInputTransactionConfig::runtime(),
            pipe_sink_executable: None,
            pipe_ready_timeout: Duration::from_secs(2),
            pipe_completion_timeout: Duration::from_secs(2),
        }
    }
}

impl<R: CommandRunner> TmuxAdapter<R> {
    pub fn with_runner(runner: R) -> Self {
        Self {
            runner,
            input_config: TmuxInputTransactionConfig::runtime(),
            pipe_sink_executable: None,
            pipe_ready_timeout: Duration::from_secs(2),
            pipe_completion_timeout: Duration::from_secs(2),
        }
    }

    #[doc(hidden)]
    pub fn with_pipe_sink_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.pipe_sink_executable = Some(executable.into());
        self
    }

    #[doc(hidden)]
    pub fn with_pipe_capture_timeouts(
        mut self,
        ready_timeout: Duration,
        completion_timeout: Duration,
    ) -> Self {
        self.pipe_ready_timeout = ready_timeout;
        self.pipe_completion_timeout = completion_timeout;
        self
    }

    pub fn with_input_transaction_config(
        mut self,
        input_config: TmuxInputTransactionConfig,
    ) -> Self {
        self.input_config = input_config;
        self
    }

    pub fn wait_for_pane_text(
        &self,
        metadata: &TmuxActivationMetadata,
        pattern: &str,
        timeout: Duration,
    ) -> Result<(), TmuxError> {
        self.validate_exact_pane(metadata)?;
        let pane = TmuxPane::new_in_session(
            metadata.session_id(),
            metadata.window_id(),
            metadata.activation_id(),
            metadata.pane_id(),
        );
        let started = Instant::now();

        loop {
            if self.capture_pane(&pane)?.contains(pattern) {
                return Ok(());
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Err(TmuxError::AgentReadinessTimeout {
                    timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                });
            }
            sleep_if_needed(Duration::from_millis(100).min(timeout - elapsed));
        }
    }

    pub fn supports_external_driver_launch(&self) -> bool {
        self.runner.supports_external_driver_launch()
    }

    pub fn ensure_session(&self, session_id: impl Into<String>) -> Result<TmuxSession, TmuxError> {
        let session = TmuxSession::new(session_id);
        validate_session_id(session.id())?;
        let check = self
            .runner
            .run(argv(["tmux", "has-session", "-t"], [session.id()]))?;

        if check.is_success() {
            return Ok(session);
        }

        self.run_checked(argv(["tmux", "new-session", "-d", "-s"], [session.id()]))?;
        Ok(session)
    }

    pub fn has_session(&self, session: &TmuxSession) -> Result<bool, TmuxError> {
        validate_session_id(session.id())?;
        let check = self
            .runner
            .run(argv(["tmux", "has-session", "-t"], [session.id()]))?;
        Ok(check.is_success())
    }

    fn create_session_with_window(
        &self,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
    ) -> Result<(TmuxSession, TmuxWindow), TmuxError> {
        let session = TmuxSession::new(session_id);
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let output = self.run_checked(argv(
            [
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
                "-s",
                session.id(),
                "-n",
            ],
            [window_name.as_str()],
        ))?;
        let window_id = trimmed_stdout(&output, "window id")?;
        let window = TmuxWindow::new_named(session.id(), run_id, window_name, window_id);

        Ok((session, window))
    }

    pub fn create_window(
        &self,
        session: &TmuxSession,
        run_id: impl Into<String>,
    ) -> Result<TmuxWindow, TmuxError> {
        let run_id = run_id.into();
        self.create_window_named(session, run_id.clone(), run_id)
    }

    pub fn create_window_named(
        &self,
        session: &TmuxSession,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
    ) -> Result<TmuxWindow, TmuxError> {
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let output = self.run_checked(argv(
            [
                "tmux",
                "new-window",
                "-P",
                "-F",
                "#{window_id}",
                "-t",
                session.id(),
                "-n",
            ],
            [window_name.as_str()],
        ))?;
        let window_id = trimmed_stdout(&output, "window id")?;

        Ok(TmuxWindow::new_named(
            session.id(),
            run_id,
            window_name,
            window_id,
        ))
    }

    fn ensure_window_named(
        &self,
        session: &TmuxSession,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
    ) -> Result<TmuxWindow, TmuxError> {
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let output = self.run_checked(argv(
            ["tmux", "list-windows", "-t", session.id(), "-F"],
            ["#{window_name}|#{window_id}"],
        ))?;

        if let Some(window_id) = listed_window_id(&output, &window_name)? {
            return Ok(TmuxWindow::new_named(
                session.id(),
                run_id,
                window_name,
                window_id,
            ));
        }

        self.create_window_named(session, run_id, window_name)
    }

    fn prepare_activation_window(
        &self,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
    ) -> Result<(TmuxSession, TmuxWindow), TmuxError> {
        let session = TmuxSession::new(session_id);
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let check = self
            .runner
            .run(argv(["tmux", "has-session", "-t"], [session.id()]))?;

        if check.is_success() {
            let window = self.ensure_window_named(&session, run_id, window_name)?;
            return Ok((session, window));
        }

        self.create_session_with_window(session.id(), run_id, window_name)
    }

    pub fn kill_pane(&self, pane: &TmuxPane) -> Result<(), TmuxError> {
        validate_owned_session_id("pane", pane.session_id())?;
        let target = pane_target(pane);
        let result = self.run_checked(argv(["tmux", "kill-pane", "-t"], [target.as_str()]));
        if result.is_ok() {
            self.runner.pipe_sink_producer_closed(&target);
        }
        result.map(|_| ())
    }

    pub fn kill_window(&self, window: &TmuxWindow) -> Result<(), TmuxError> {
        validate_owned_session_id("window", window.session_id())?;
        let target = window_target(window);
        self.run_checked(argv(["tmux", "kill-window", "-t"], [target.as_str()]))?;
        Ok(())
    }

    pub fn kill_session(&self, session: &TmuxSession) -> Result<(), TmuxError> {
        validate_owned_session_id("session", session.id())?;
        self.run_checked(argv(["tmux", "kill-session", "-t"], [session.id()]))?;
        Ok(())
    }

    pub fn send_keys_literal(&self, pane: &TmuxPane, text: &str) -> Result<(), TmuxError> {
        validate_owned_session_id("pane", pane.session_id())?;
        let target = pane_target(pane);
        self.run_checked(argv(
            ["tmux", "send-keys", "-t", target.as_str(), "-l"],
            [text],
        ))?;
        Ok(())
    }

    fn paste_keys_literal(
        &self,
        pane: &TmuxPane,
        text: &str,
        buffer_name: &str,
    ) -> Result<(), TmuxError> {
        validate_owned_session_id("pane", pane.session_id())?;
        let target = pane_target(pane);
        self.run_checked(argv(
            ["tmux", "set-buffer", "-b", buffer_name, "--"],
            [text],
        ))?;
        self.run_checked(argv(
            ["tmux", "paste-buffer", "-p", "-d", "-b", buffer_name, "-t"],
            [target.as_str()],
        ))?;
        Ok(())
    }

    pub fn send_key(&self, pane: &TmuxPane, key: &str) -> Result<(), TmuxError> {
        validate_owned_session_id("pane", pane.session_id())?;
        let target = pane_target(pane);
        self.run_checked(argv(["tmux", "send-keys", "-t", target.as_str()], [key]))?;
        Ok(())
    }

    pub fn capture_pane(&self, pane: &TmuxPane) -> Result<String, TmuxError> {
        validate_owned_session_id("pane", pane.session_id())?;
        let target = pane_target(pane);
        let output = self.run_checked(argv(
            ["tmux", "capture-pane", "-p", "-t"],
            [target.as_str()],
        ))?;
        Ok(output.stdout)
    }

    pub fn start_pipe(&self, pane: &TmuxPane, path: &Path) -> Result<(), TmuxError> {
        let pipe_sink = vec!["pipe-sink".to_string()];
        ensure_durable_pipe_capture_supported()
            .map_err(|err| TmuxError::io(&pipe_sink, &err.to_string()))?;
        let root = path
            .parent()
            .ok_or_else(|| TmuxError::io(&pipe_sink, "transcript path must have a parent"))?;
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                TmuxError::io(&pipe_sink, "transcript path must have a utf-8 file name")
            })?;
        let identity =
            pipe_sink_identity(path).map_err(|err| TmuxError::io(&pipe_sink, &err.to_string()))?;
        self.start_pipe_capture(
            pane,
            root,
            file_name,
            &identity,
            format!(".{file_name}.ready"),
        )
    }

    pub fn start_pipe_capture(
        &self,
        pane: &TmuxPane,
        root: &Path,
        relative_path: impl AsRef<Path>,
        identity: &PipeSinkIdentity,
        ack_relative_path: impl AsRef<Path>,
    ) -> Result<(), TmuxError> {
        let ack_relative_path = ack_relative_path.as_ref();
        let completion_relative_path = default_pipe_completion_path(ack_relative_path);
        self.start_pipe_capture_with_completion(
            pane,
            root,
            relative_path,
            identity,
            ack_relative_path,
            completion_relative_path,
        )
        .map(|_| ())
    }

    pub fn start_pipe_capture_with_completion(
        &self,
        pane: &TmuxPane,
        root: &Path,
        relative_path: impl AsRef<Path>,
        identity: &PipeSinkIdentity,
        ack_relative_path: impl AsRef<Path>,
        completion_relative_path: impl AsRef<Path>,
    ) -> Result<TmuxPipeCapture, TmuxError> {
        let ack_nonce = new_pipe_capture_nonce();
        let request = PipeCaptureRequest {
            root,
            transcript_relative_path: relative_path.as_ref(),
            identity,
            ack_relative_path: ack_relative_path.as_ref(),
            completion_relative_path: completion_relative_path.as_ref(),
            ack_nonce: &ack_nonce,
            preserve_ready_ack: false,
        };
        self.start_pipe_capture_request(pane, &request)
    }

    pub(crate) fn start_pipe_capture_with_completion_nonce(
        &self,
        pane: &TmuxPane,
        request: &PipeCaptureRequest<'_>,
    ) -> Result<TmuxPipeCapture, TmuxError> {
        self.start_pipe_capture_request(pane, request)
    }

    fn start_pipe_capture_request(
        &self,
        pane: &TmuxPane,
        request: &PipeCaptureRequest<'_>,
    ) -> Result<TmuxPipeCapture, TmuxError> {
        let pipe_sink = vec!["pipe-sink".to_string()];
        ensure_durable_pipe_capture_supported()
            .map_err(|err| TmuxError::io(&pipe_sink, &err.to_string()))?;
        validate_owned_session_id("pane", pane.session_id())?;
        let target = pane_target(pane);
        let sink = match &self.pipe_sink_executable {
            Some(executable) => executable.clone(),
            None => {
                let current_exe = vec!["current-exe".to_string()];
                env::current_exe().map_err(|err| TmuxError::io(&current_exe, &err.to_string()))?
            }
        };
        let command = format!(
            "{} --pipe-sink --root {} --relative {} --dev {} --ino {} --uid {} --mode {} --nlink {} --ack-relative {} --completion-relative {} --ack-nonce {}",
            shell_single_quote(&sink),
            shell_single_quote(request.root),
            shell_single_quote(request.transcript_relative_path),
            request.identity.dev,
            request.identity.ino,
            request.identity.uid,
            request.identity.mode,
            request.identity.nlink,
            shell_single_quote(request.ack_relative_path),
            shell_single_quote(request.completion_relative_path),
            shell_single_quote_str(request.ack_nonce)
        );
        let argv = argv(
            ["tmux", "pipe-pane", "-o", "-t", target.as_str()],
            [command.as_str()],
        );
        let redacted_argv = pipe_sink_redacted_argv(target.as_str());
        self.run_checked(argv.clone())
            .map_err(|err| redact_pipe_sink_error(err, &redacted_argv))?;
        let ready = wait_for_pipe_ack(request, &sink, &redacted_argv, self.pipe_ready_timeout)?;
        Ok(TmuxPipeCapture {
            root: request.root.to_path_buf(),
            transcript_relative_path: request.transcript_relative_path.to_path_buf(),
            completion_relative_path: request.completion_relative_path.to_path_buf(),
            transcript_identity: *request.identity,
            nonce: request.ack_nonce.to_string(),
            helper_pid: ready.pid,
            helper_process_start_time_ticks: ready.process_start_time_ticks,
            external_helper: self.runner.pipe_sink_helper_is_external(),
        })
    }

    pub fn wait_for_pipe_capture_completion(
        &self,
        capture: &TmuxPipeCapture,
    ) -> Result<PipeSinkCompletionPayload, TmuxError> {
        let completion = self.wait_for_pipe_capture_completion_preserve(capture)?;
        self.remove_pipe_capture_completion(capture)?;
        Ok(completion)
    }

    pub(crate) fn wait_for_pipe_capture_completion_preserve(
        &self,
        capture: &TmuxPipeCapture,
    ) -> Result<PipeSinkCompletionPayload, TmuxError> {
        let argv = vec!["pipe-sink".to_string(), "<completion-redacted>".to_string()];
        let deadline = Instant::now() + self.pipe_completion_timeout;
        let completion = loop {
            match verify_pipe_sink_completion_under_root(
                &capture.root,
                &capture.completion_relative_path,
                &capture.transcript_relative_path,
                &capture.nonce,
                &capture.transcript_identity,
                capture.helper_pid,
                capture.helper_process_start_time_ticks,
            ) {
                Ok(completion) => break completion,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    if capture.external_helper
                        && !helper_process_matches(
                            capture.helper_pid,
                            capture.helper_process_start_time_ticks,
                        )
                        .map_err(|_| pipe_completion_error(&argv))?
                    {
                        return Err(pipe_completion_error(&argv));
                    }
                }
                Err(_) => {
                    let _ = wait_for_pipe_helper_exit(capture, deadline);
                    return Err(pipe_completion_error(&argv));
                }
            }
            if Instant::now() >= deadline {
                return Err(pipe_completion_error(&argv));
            }
            thread::sleep(Duration::from_millis(10));
        };
        self.finish_pipe_capture_completion(capture, completion, deadline, &argv)
    }

    pub(crate) fn pipe_capture_completion_if_ready(
        &self,
        capture: &TmuxPipeCapture,
    ) -> Result<Option<PipeSinkCompletionPayload>, TmuxError> {
        let argv = vec!["pipe-sink".to_string(), "<completion-redacted>".to_string()];
        let completion = match verify_pipe_sink_completion_under_root(
            &capture.root,
            &capture.completion_relative_path,
            &capture.transcript_relative_path,
            &capture.nonce,
            &capture.transcript_identity,
            capture.helper_pid,
            capture.helper_process_start_time_ticks,
        ) {
            Ok(completion) => completion,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(pipe_completion_error(&argv)),
        };
        let deadline = Instant::now() + self.pipe_completion_timeout;
        self.finish_pipe_capture_completion(capture, completion, deadline, &argv)
            .map(Some)
    }

    fn finish_pipe_capture_completion(
        &self,
        capture: &TmuxPipeCapture,
        completion: PipeSinkCompletionPayload,
        deadline: Instant,
        argv: &[String],
    ) -> Result<PipeSinkCompletionPayload, TmuxError> {
        wait_for_pipe_helper_exit(capture, deadline).map_err(|_| pipe_completion_error(argv))?;
        Ok(completion)
    }

    pub(crate) fn remove_pipe_capture_completion(
        &self,
        capture: &TmuxPipeCapture,
    ) -> Result<(), TmuxError> {
        let argv = vec!["pipe-sink".to_string(), "<completion-redacted>".to_string()];
        remove_pipe_sink_ack_under_root(&capture.root, &capture.completion_relative_path)
            .map_err(|_| pipe_completion_error(&argv))
    }

    pub(crate) fn validate_exact_pane(
        &self,
        metadata: &TmuxActivationMetadata,
    ) -> Result<(), TmuxError> {
        validate_owned_session_id("pane", metadata.session_id())?;
        let target = metadata_pane_target(metadata);
        let output = self.run_checked(argv(
            ["tmux", "display-message", "-p", "-t", target.as_str()],
            ["#{session_name}|#{window_id}|#{window_name}|#{pane_id}"],
        ))?;
        let (actual_session_id, actual_window_id, actual_window_name, actual_pane_id) =
            pane_identity_stdout(&output)?;

        if actual_session_id != metadata.session_id()
            || actual_window_id != metadata.window_id()
            || actual_window_name != metadata.window_name()
            || actual_pane_id != metadata.pane_id()
        {
            return Err(TmuxError::PaneMetadataMismatch(Box::new(
                TmuxPaneMetadataMismatch::new(
                    TmuxPaneIdentity::new(
                        metadata.session_id(),
                        metadata.window_id(),
                        metadata.window_name(),
                        metadata.pane_id(),
                    ),
                    TmuxPaneIdentity::new(
                        actual_session_id,
                        actual_window_id,
                        actual_window_name,
                        actual_pane_id,
                    ),
                ),
            )));
        }

        Ok(())
    }

    fn run_checked(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        let output = self.runner.run(argv.clone())?;
        if output.is_success() {
            Ok(output)
        } else {
            Err(TmuxError::command_failed(
                &argv,
                output.status,
                &output.stderr,
            ))
        }
    }
}

impl<R: CommandRunner> AgentLifecycleAdapter for TmuxAdapter<R> {
    type ActivationRequest = TmuxActivationRequest;
    type Activation = TmuxActivation;
    type Handle = TmuxAgentHandle;
    type Observation = TmuxLifecycleObservation;
    type Error = TmuxError;

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities::tmux_lifecycle()
    }

    fn prepare_activation(
        &self,
        request: Self::ActivationRequest,
    ) -> Result<Self::Activation, Self::Error> {
        let (session, window) = self.prepare_activation_window(
            request.session_id(),
            request.run_id(),
            request.window_name(),
        )?;
        let pane = self.split_pane_for_activation(&window, request.activation_id())?;
        Ok(TmuxActivation::new(session, window, pane))
    }

    fn start_agent(
        &self,
        activation: &Self::Activation,
        command: &str,
    ) -> Result<Self::Handle, Self::Error> {
        self.send_input_transaction_with_submit_key_count(activation.metadata(), command, 1)?;
        Ok(activation.clone().into_handle())
    }

    fn send_prompt(&self, handle: &Self::Handle, prompt: &str) -> Result<(), Self::Error> {
        self.send_input_transaction_with_submit_key_count(handle.metadata(), prompt, 2)?;
        Ok(())
    }

    fn observe_lifecycle(&self, handle: &Self::Handle) -> Result<Self::Observation, Self::Error> {
        let captured_text = self.capture_pane(handle.pane())?;
        Ok(TmuxLifecycleObservation::new(
            handle.metadata().clone(),
            captured_text,
            LifecycleStatus::Running,
        ))
    }

    fn cleanup_activation(
        &self,
        handle: &Self::Handle,
        status: LifecycleStatus,
    ) -> Result<LifecycleCleanup, Self::Error> {
        let action = match status {
            LifecycleStatus::ContractSatisfied => {
                self.kill_pane(handle.pane())?;
                LifecycleCleanupAction::KillPane
            }
            LifecycleStatus::Running | LifecycleStatus::Blocked | LifecycleStatus::Failed => {
                LifecycleCleanupAction::PreservePane
            }
        };

        Ok(LifecycleCleanup::new(action, status))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxActivationRequest {
    session_id: String,
    run_id: String,
    window_name: String,
    activation_id: String,
}

impl TmuxActivationRequest {
    pub fn new(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
        activation_id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            run_id: run_id.into(),
            window_name: window_name.into(),
            activation_id: activation_id.into(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn window_name(&self) -> &str {
        &self.window_name
    }

    pub fn activation_id(&self) -> &str {
        &self.activation_id
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxActivation {
    session: TmuxSession,
    window: TmuxWindow,
    pane: TmuxPane,
    metadata: TmuxActivationMetadata,
}

impl TmuxActivation {
    pub fn new(session: TmuxSession, window: TmuxWindow, pane: TmuxPane) -> Self {
        let metadata = TmuxActivationMetadata::from_tmux(&session, &window, &pane);
        Self {
            session,
            window,
            pane,
            metadata,
        }
    }

    pub fn session(&self) -> &TmuxSession {
        &self.session
    }

    pub fn window(&self) -> &TmuxWindow {
        &self.window
    }

    pub fn pane(&self) -> &TmuxPane {
        &self.pane
    }

    pub fn metadata(&self) -> &TmuxActivationMetadata {
        &self.metadata
    }

    pub fn into_handle(self) -> TmuxAgentHandle {
        TmuxAgentHandle {
            pane: self.pane,
            metadata: self.metadata,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxAgentHandle {
    pane: TmuxPane,
    metadata: TmuxActivationMetadata,
}

impl TmuxAgentHandle {
    pub fn pane(&self) -> &TmuxPane {
        &self.pane
    }

    pub fn metadata(&self) -> &TmuxActivationMetadata {
        &self.metadata
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxActivationMetadata {
    session_id: String,
    run_id: String,
    window_name: String,
    window_id: String,
    activation_id: String,
    pane_id: String,
    allocation_generation: u64,
}

impl TmuxActivationMetadata {
    pub fn new(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
        window_id: impl Into<String>,
        activation_id: impl Into<String>,
        pane_id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            run_id: run_id.into(),
            window_name: window_name.into(),
            window_id: window_id.into(),
            activation_id: activation_id.into(),
            pane_id: pane_id.into(),
            allocation_generation: 0,
        }
    }

    pub fn with_allocation_generation(mut self, allocation_generation: u64) -> Self {
        self.allocation_generation = allocation_generation;
        self
    }

    pub fn from_tmux(session: &TmuxSession, window: &TmuxWindow, pane: &TmuxPane) -> Self {
        Self::new(
            session.id(),
            window.run_id(),
            window.name(),
            window.id(),
            pane.activation_id(),
            pane.id(),
        )
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn window_name(&self) -> &str {
        &self.window_name
    }

    pub fn window_id(&self) -> &str {
        &self.window_id
    }

    pub fn activation_id(&self) -> &str {
        &self.activation_id
    }

    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    pub fn allocation_generation(&self) -> u64 {
        self.allocation_generation
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxLifecycleObservation {
    metadata: TmuxActivationMetadata,
    captured_text: String,
    status: LifecycleStatus,
}

impl TmuxLifecycleObservation {
    pub fn new(
        metadata: TmuxActivationMetadata,
        captured_text: impl Into<String>,
        status: LifecycleStatus,
    ) -> Self {
        Self {
            metadata,
            captured_text: captured_text.into(),
            status,
        }
    }

    pub fn metadata(&self) -> &TmuxActivationMetadata {
        &self.metadata
    }

    pub fn captured_text(&self) -> &str {
        &self.captured_text
    }

    pub fn status(&self) -> LifecycleStatus {
        self.status
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct TmuxSession {
    id: String,
}

impl TmuxSession {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct TmuxWindow {
    session_id: String,
    run_id: String,
    name: String,
    id: String,
}

impl TmuxWindow {
    pub fn new(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        let run_id = run_id.into();
        Self {
            session_id: session_id.into(),
            name: run_id.clone(),
            run_id,
            id: id.into(),
        }
    }

    pub fn new_named(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        name: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            run_id: run_id.into(),
            name: name.into(),
            id: id.into(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct TmuxPane {
    session_id: String,
    window_id: String,
    activation_id: String,
    id: String,
}

impl TmuxPane {
    pub fn new(
        window_id: impl Into<String>,
        activation_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: String::new(),
            window_id: window_id.into(),
            activation_id: activation_id.into(),
            id: id.into(),
        }
    }

    pub fn new_in_session(
        session_id: impl Into<String>,
        window_id: impl Into<String>,
        activation_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            window_id: window_id.into(),
            activation_id: activation_id.into(),
            id: id.into(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn window_id(&self) -> &str {
        &self.window_id
    }

    pub fn activation_id(&self) -> &str {
        &self.activation_id
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct PaneActivation {
    activation_id: String,
    pane_id: String,
}

impl PaneActivation {
    pub fn new(activation_id: impl Into<String>, pane_id: impl Into<String>) -> Self {
        Self {
            activation_id: activation_id.into(),
            pane_id: pane_id.into(),
        }
    }

    pub fn activation_id(&self) -> &str {
        &self.activation_id
    }

    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }
}

pub trait CommandRunner: Clone {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError>;

    #[doc(hidden)]
    fn pipe_sink_helper_is_external(&self) -> bool {
        true
    }

    #[doc(hidden)]
    fn supports_external_driver_launch(&self) -> bool {
        false
    }

    #[doc(hidden)]
    fn pipe_sink_producer_closed(&self, _target: &str) {}
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            status: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    pub fn failure(stderr: impl Into<String>) -> Self {
        Self {
            status: 1,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    pub fn success_status(
        status: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            status,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    pub fn is_success(&self) -> bool {
        self.status == 0
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn supports_external_driver_launch(&self) -> bool {
        true
    }

    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        let Some((program, args)) = argv.split_first() else {
            return Err(TmuxError::EmptyArgv);
        };

        let tmux_binary = env::var_os("HUMANIZE_TMUX_BIN").filter(|value| !value.is_empty());
        let command_program = if is_tmux_program(program) {
            tmux_binary
                .as_deref()
                .unwrap_or_else(|| std::ffi::OsStr::new(program))
        } else {
            std::ffi::OsStr::new(program)
        };

        let output = Command::new(command_program)
            .args(args)
            .output()
            .map_err(|err| TmuxError::io(&argv, &err.to_string()))?;
        let status = output.status.code().unwrap_or(-1);

        Ok(CommandOutput {
            status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxPaneIdentity {
    pub session_id: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
}

impl TmuxPaneIdentity {
    pub fn new(
        session_id: impl Into<String>,
        window_id: impl Into<String>,
        window_name: impl Into<String>,
        pane_id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            window_id: window_id.into(),
            window_name: window_name.into(),
            pane_id: pane_id.into(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxPaneMetadataMismatch {
    pub expected: TmuxPaneIdentity,
    pub actual: TmuxPaneIdentity,
}

impl TmuxPaneMetadataMismatch {
    pub fn new(expected: TmuxPaneIdentity, actual: TmuxPaneIdentity) -> Self {
        Self { expected, actual }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxCommandDiagnostic {
    pub operation: String,
    pub command_hash: String,
    pub command_length: u64,
    pub detail_hash: String,
    pub detail_length: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TmuxError {
    EmptyArgv,
    MissingSession {
        target: &'static str,
    },
    InvalidSessionName {
        reason: &'static str,
    },
    InvalidOperationId,
    ReservedSession {
        session_id: String,
    },
    EmptyOutput {
        field: &'static str,
    },
    Io {
        diagnostic: TmuxCommandDiagnostic,
    },
    InputLedger {
        diagnostic: TmuxCommandDiagnostic,
    },
    AgentReadinessTimeout {
        timeout_ms: u64,
    },
    PaneMetadataMismatch(Box<TmuxPaneMetadataMismatch>),
    CommandFailed {
        diagnostic: TmuxCommandDiagnostic,
        status: i32,
    },
}

impl TmuxError {
    pub fn io(argv: &[String], detail: &str) -> Self {
        Self::Io {
            diagnostic: TmuxCommandDiagnostic::new(argv, detail),
        }
    }

    pub fn command_failed(argv: &[String], status: i32, detail: &str) -> Self {
        Self::CommandFailed {
            diagnostic: TmuxCommandDiagnostic::new(argv, detail),
            status,
        }
    }

    fn input_ledger(detail: &str) -> Self {
        Self::InputLedger {
            diagnostic: TmuxCommandDiagnostic::for_operation("input-ledger", &[], detail),
        }
    }
}

impl TmuxCommandDiagnostic {
    fn new(argv: &[String], detail: &str) -> Self {
        Self::for_operation(restricted_operation(argv), argv, detail)
    }

    fn for_operation(operation: &str, argv: &[String], detail: &str) -> Self {
        Self {
            operation: operation.to_string(),
            command_hash: hash_arguments(argv),
            command_length: argv.iter().map(|argument| argument.len() as u64).sum(),
            detail_hash: hash_text(detail),
            detail_length: detail.len() as u64,
        }
    }
}

impl fmt::Display for TmuxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyArgv => write!(formatter, "empty command argv"),
            Self::MissingSession { target } => {
                write!(formatter, "tmux {target} requires session ownership")
            }
            Self::InvalidSessionName { reason } => write!(formatter, "{reason}"),
            Self::InvalidOperationId => write!(
                formatter,
                "tmux operation id must contain only ASCII letters, digits, '-' or '_'"
            ),
            Self::ReservedSession { session_id } => {
                write!(formatter, "tmux session named {session_id} is reserved")
            }
            Self::EmptyOutput { field } => write!(formatter, "tmux did not return {field}"),
            Self::AgentReadinessTimeout { timeout_ms } => write!(
                formatter,
                "tmux pane did not reach configured readiness within {timeout_ms} ms"
            ),
            Self::Io { diagnostic } => {
                write!(formatter, "tmux I/O failure ({diagnostic})")
            }
            Self::InputLedger { diagnostic } => {
                write!(formatter, "tmux input ledger failure ({diagnostic})")
            }
            Self::PaneMetadataMismatch(mismatch) => write!(
                formatter,
                "tmux pane metadata mismatch: expected {}:{}({}).{}, got {}:{}({}).{}",
                mismatch.expected.session_id,
                mismatch.expected.window_id,
                mismatch.expected.window_name,
                mismatch.expected.pane_id,
                mismatch.actual.session_id,
                mismatch.actual.window_id,
                mismatch.actual.window_name,
                mismatch.actual.pane_id,
            ),
            Self::CommandFailed { diagnostic, status } => {
                write!(
                    formatter,
                    "tmux command failed with status {status} ({diagnostic})"
                )
            }
        }
    }
}

impl fmt::Display for TmuxCommandDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "operation={} command_hash={} command_length={} detail_hash={} detail_length={}",
            self.operation,
            self.command_hash,
            self.command_length,
            self.detail_hash,
            self.detail_length
        )
    }
}

impl Error for TmuxError {}

fn argv<const N: usize, const M: usize>(head: [&str; N], tail: [&str; M]) -> Vec<String> {
    head.into_iter().chain(tail).map(String::from).collect()
}

fn is_tmux_program(value: &str) -> bool {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "tmux")
}

fn restricted_operation(argv: &[String]) -> &'static str {
    let operation = if argv.first().is_some_and(|program| is_tmux_program(program)) {
        argv.get(1).map(String::as_str)
    } else {
        argv.first()
            .and_then(|program| Path::new(program).file_name())
            .and_then(|name| name.to_str())
    };
    match operation {
        Some("capture-pane") => "capture-pane",
        Some("current-exe") => "current-exe",
        Some("display-message") => "display-message",
        Some("has-session") => "has-session",
        Some("kill-pane") => "kill-pane",
        Some("kill-session") => "kill-session",
        Some("kill-window") => "kill-window",
        Some("list-panes") => "list-panes",
        Some("new-session") => "new-session",
        Some("new-window") => "new-window",
        Some("pipe-pane") => "pipe-pane",
        Some("pipe-sink") => "pipe-sink",
        Some("paste-buffer") => "paste-buffer",
        Some("send-keys") => "send-keys",
        Some("set-buffer") => "set-buffer",
        Some("split-window") => "split-window",
        _ => "command",
    }
}

fn hash_arguments(argv: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((argv.len() as u64).to_be_bytes());
    for argument in argv {
        hasher.update((argument.len() as u64).to_be_bytes());
        hasher.update(argument.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn validate_session_id(session_id: &str) -> Result<(), TmuxError> {
    if session_id.is_empty() {
        return Err(TmuxError::InvalidSessionName {
            reason: "tmux session name must not be empty",
        });
    }
    if session_id.contains(':') || session_id.contains('.') {
        return Err(TmuxError::InvalidSessionName {
            reason: "tmux session name must not contain tmux target delimiters ':' or '.'",
        });
    }
    if session_id == "dev" {
        Err(TmuxError::ReservedSession {
            session_id: session_id.to_string(),
        })
    } else {
        Ok(())
    }
}

fn validate_owned_session_id(target: &'static str, session_id: &str) -> Result<(), TmuxError> {
    if session_id.is_empty() {
        Err(TmuxError::MissingSession { target })
    } else {
        validate_session_id(session_id)
    }
}

fn window_target(window: &TmuxWindow) -> String {
    format!("{}:{}", window.session_id(), window.id())
}

fn pane_target(pane: &TmuxPane) -> String {
    format!("{}:{}.{}", pane.session_id(), pane.window_id(), pane.id())
}

fn metadata_pane_target(metadata: &TmuxActivationMetadata) -> String {
    format!(
        "{}:{}.{}",
        metadata.session_id(),
        metadata.window_id(),
        metadata.pane_id()
    )
}

fn sleep_if_needed(duration: Duration) {
    if !duration.is_zero() {
        thread::sleep(duration);
    }
}

fn trimmed_stdout(output: &CommandOutput, field: &'static str) -> Result<String, TmuxError> {
    let value = output.stdout.trim();
    if value.is_empty() {
        Err(TmuxError::EmptyOutput { field })
    } else {
        Ok(value.to_string())
    }
}

fn listed_window_id(
    output: &CommandOutput,
    window_name: &str,
) -> Result<Option<String>, TmuxError> {
    for line in output.stdout.lines() {
        let fields = tmux_record_fields(line);
        let [name, window_id] = fields.as_slice() else {
            continue;
        };
        if *name == window_name {
            let window_id = window_id.trim();
            if window_id.is_empty() {
                return Err(TmuxError::EmptyOutput { field: "window id" });
            }
            return Ok(Some(window_id.to_string()));
        }
    }

    Ok(None)
}

fn window_pane_stdout(output: &CommandOutput) -> Result<(String, String), TmuxError> {
    let value = output.stdout.trim();
    if value.is_empty() {
        return Err(TmuxError::EmptyOutput {
            field: "window and pane ids",
        });
    }

    let fields = tmux_record_fields(value);
    let mut fields = fields.into_iter();
    let Some(window_id) = fields.next() else {
        return Err(TmuxError::EmptyOutput { field: "window id" });
    };
    let Some(pane_id) = fields.next() else {
        return Err(TmuxError::EmptyOutput { field: "pane id" });
    };

    Ok((window_id.to_string(), pane_id.to_string()))
}

fn pane_identity_stdout(
    output: &CommandOutput,
) -> Result<(String, String, String, String), TmuxError> {
    let value = output.stdout.trim();
    if value.is_empty() {
        return Err(TmuxError::EmptyOutput {
            field: "pane metadata",
        });
    }

    pane_identity_text(value)
}

fn pane_identity_text(value: &str) -> Result<(String, String, String, String), TmuxError> {
    let fields = tmux_record_fields(value);
    let mut fields = fields.into_iter();
    let Some(session_id) = fields.next() else {
        return Err(TmuxError::EmptyOutput {
            field: "session name",
        });
    };
    let Some(window_id) = fields.next() else {
        return Err(TmuxError::EmptyOutput { field: "window id" });
    };
    let Some(window_name) = fields.next() else {
        return Err(TmuxError::EmptyOutput {
            field: "window name",
        });
    };
    let Some(pane_id) = fields.next() else {
        return Err(TmuxError::EmptyOutput { field: "pane id" });
    };

    Ok((
        session_id.to_string(),
        window_id.to_string(),
        window_name.to_string(),
        pane_id.to_string(),
    ))
}

fn tmux_record_fields(value: &str) -> Vec<&str> {
    if value.contains('|') {
        value.split('|').collect()
    } else if value.contains('\t') {
        value.split('\t').collect()
    } else {
        value.split_whitespace().collect()
    }
}
