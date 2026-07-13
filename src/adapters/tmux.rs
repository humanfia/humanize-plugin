use std::env;
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::adapters::lifecycle::{
    AdapterCapabilities, AgentLifecycleAdapter, LifecycleCleanup, LifecycleCleanupAction,
    LifecycleStatus,
};
use crate::input_ledger::{
    MachineInputClock, MachineInputLedger, MachineInputRecord, MachineInputSubmission,
    machine_input_payload_hash, machine_input_transaction_id, normalize_machine_input_text,
};
use crate::pipe_sink::{
    PipeSinkCompletionPayload, PipeSinkIdentity, PipeSinkReady,
    ensure_durable_pipe_capture_supported, pipe_sink_identity, remove_pipe_sink_ack_under_root,
    verify_pipe_sink_completion_under_root, verify_pipe_sink_ready_ack_under_root,
};

#[derive(Debug, Clone)]
pub struct TmuxAdapter<R: CommandRunner = SystemCommandRunner> {
    runner: R,
    input_config: TmuxInputTransactionConfig,
    agent_startup_delay: Duration,
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
            agent_startup_delay: Duration::from_secs(5),
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
            agent_startup_delay: Duration::ZERO,
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

    pub fn wait_for_agent_startup(&self) {
        sleep_if_needed(self.agent_startup_delay);
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
                    pattern: pattern.to_string(),
                    timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                });
            }
            sleep_if_needed(Duration::from_millis(100).min(timeout - elapsed));
        }
    }

    pub fn create_session_with_window_pane(
        &self,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
        activation_id: impl Into<String>,
    ) -> Result<(TmuxSession, TmuxWindow, TmuxPane), TmuxError> {
        let session = TmuxSession::new(session_id);
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let activation_id = activation_id.into();
        let output = self.run_checked(argv(
            [
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                session.id(),
                "-n",
            ],
            [window_name.as_str()],
        ))?;
        let (window_id, pane_id) = window_pane_stdout(&output)?;
        let window = TmuxWindow::new_named(session.id(), run_id, window_name, window_id);
        let pane = TmuxPane::new_in_session(
            session.id(),
            window.id(),
            activation_id.as_str(),
            pane_id.as_str(),
        );

        Ok((session, window, pane))
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

    pub fn create_window_named_with_pane(
        &self,
        session: &TmuxSession,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
        activation_id: impl Into<String>,
    ) -> Result<(TmuxWindow, TmuxPane), TmuxError> {
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let activation_id = activation_id.into();
        let output = self.run_checked(argv(
            [
                "tmux",
                "new-window",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-t",
                session.id(),
                "-n",
            ],
            [window_name.as_str()],
        ))?;
        let (window_id, pane_id) = window_pane_stdout(&output)?;
        let window = TmuxWindow::new_named(session.id(), run_id, window_name, window_id);
        let pane = TmuxPane::new_in_session(
            session.id(),
            window.id(),
            activation_id.as_str(),
            pane_id.as_str(),
        );

        Ok((window, pane))
    }

    pub fn split_pane_for_activation(
        &self,
        window: &TmuxWindow,
        activation_id: impl Into<String>,
    ) -> Result<TmuxPane, TmuxError> {
        validate_owned_session_id("window", window.session_id())?;
        let activation_id = activation_id.into();
        let target = window_target(window);
        let output = self.run_checked(argv(
            [
                "tmux",
                "split-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                target.as_str(),
            ],
            ["-v"],
        ))?;
        let pane_id = trimmed_stdout(&output, "pane id")?;

        Ok(TmuxPane::new_in_session(
            window.session_id(),
            window.id(),
            activation_id,
            pane_id,
        ))
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

    pub fn send_key(&self, pane: &TmuxPane, key: &str) -> Result<(), TmuxError> {
        validate_owned_session_id("pane", pane.session_id())?;
        let target = pane_target(pane);
        self.run_checked(argv(["tmux", "send-keys", "-t", target.as_str()], [key]))?;
        Ok(())
    }

    pub fn send_input_transaction(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
    ) -> Result<TmuxInputTransaction, TmuxError> {
        self.send_input_transaction_with_submit_key_count(
            metadata,
            text,
            self.input_config.submit_key_count,
        )
    }

    pub fn send_input_transaction_with_submit_key_count(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
        submit_key_count: usize,
    ) -> Result<TmuxInputTransaction, TmuxError> {
        self.validate_exact_pane(metadata)?;
        let submit_key_count = submit_key_count.max(1);
        let started_at_ms = self.input_config.clock.now_ms();
        let normalized_text = normalize_machine_input_text(text);
        let payload_hash = machine_input_payload_hash(&normalized_text);
        let sequence = self.input_config.ledger.next_sequence();
        let transaction_id = machine_input_transaction_id(
            metadata.run_id(),
            metadata.activation_id(),
            metadata.pane_id(),
            &payload_hash,
            started_at_ms,
            sequence,
        );
        let pane = TmuxPane::new_in_session(
            metadata.session_id(),
            metadata.window_id(),
            metadata.activation_id(),
            metadata.pane_id(),
        );

        self.input_config
            .ledger
            .append(MachineInputRecord::started(MachineInputSubmission {
                run_id: metadata.run_id(),
                activation_id: metadata.activation_id(),
                pane_id: metadata.pane_id(),
                started_at_ms,
                submitted_at_ms: started_at_ms,
                text,
                submit_key_count,
                transaction_id: transaction_id.clone(),
            }))
            .map_err(|err| TmuxError::InputLedger {
                message: err.to_string(),
            })?;

        if let Err(err) = self.send_keys_literal(&pane, text) {
            self.record_failed_input(
                metadata,
                text,
                started_at_ms,
                &transaction_id,
                submit_key_count,
            );
            return Err(err);
        }
        sleep_if_needed(self.input_config.prompt_to_submit_delay);
        for index in 0..submit_key_count {
            if let Err(err) = self.send_key(&pane, "Enter") {
                self.record_failed_input(
                    metadata,
                    text,
                    started_at_ms,
                    &transaction_id,
                    submit_key_count,
                );
                return Err(err);
            }
            if index + 1 < submit_key_count {
                sleep_if_needed(self.input_config.submit_key_delay);
            }
        }

        let submitted_at_ms = self.input_config.clock.now_ms();
        let record = MachineInputRecord::submitted(MachineInputSubmission {
            run_id: metadata.run_id(),
            activation_id: metadata.activation_id(),
            pane_id: metadata.pane_id(),
            started_at_ms,
            submitted_at_ms,
            text,
            submit_key_count,
            transaction_id: transaction_id.clone(),
        });
        self.input_config
            .ledger
            .append(record.clone())
            .map_err(|err| TmuxError::InputLedger {
                message: err.to_string(),
            })?;
        Ok(TmuxInputTransaction { record })
    }

    fn record_failed_input(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
        started_at_ms: u64,
        transaction_id: &str,
        submit_key_count: usize,
    ) {
        let failed_at_ms = self.input_config.clock.now_ms();
        let _ =
            self.input_config
                .ledger
                .append(MachineInputRecord::failed(MachineInputSubmission {
                    run_id: metadata.run_id(),
                    activation_id: metadata.activation_id(),
                    pane_id: metadata.pane_id(),
                    started_at_ms,
                    submitted_at_ms: failed_at_ms,
                    text,
                    submit_key_count,
                    transaction_id: transaction_id.to_string(),
                }));
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
        ensure_durable_pipe_capture_supported().map_err(|err| TmuxError::Io {
            argv: vec!["pipe-sink".to_string()],
            message: err.to_string(),
        })?;
        let root = path.parent().ok_or_else(|| TmuxError::Io {
            argv: vec!["pipe-sink".to_string()],
            message: "transcript path must have a parent".to_string(),
        })?;
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| TmuxError::Io {
                argv: vec!["pipe-sink".to_string()],
                message: "transcript path must have a utf-8 file name".to_string(),
            })?;
        let identity = pipe_sink_identity(path).map_err(|err| TmuxError::Io {
            argv: vec!["pipe-sink".to_string()],
            message: err.to_string(),
        })?;
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
        ensure_durable_pipe_capture_supported().map_err(|err| TmuxError::Io {
            argv: vec!["pipe-sink".to_string()],
            message: err.to_string(),
        })?;
        validate_owned_session_id("pane", pane.session_id())?;
        let target = pane_target(pane);
        let relative_path = relative_path.as_ref();
        let ack_relative_path = ack_relative_path.as_ref();
        let completion_relative_path = completion_relative_path.as_ref();
        let sink = match &self.pipe_sink_executable {
            Some(executable) => executable.clone(),
            None => env::current_exe().map_err(|err| TmuxError::Io {
                argv: vec!["current-exe".to_string()],
                message: err.to_string(),
            })?,
        };
        let ack_nonce = pipe_ack_nonce();
        let command = format!(
            "{} --pipe-sink --root {} --relative {} --dev {} --ino {} --uid {} --mode {} --nlink {} --ack-relative {} --completion-relative {} --ack-nonce {}",
            shell_single_quote(&sink),
            shell_single_quote(root),
            shell_single_quote(relative_path),
            identity.dev,
            identity.ino,
            identity.uid,
            identity.mode,
            identity.nlink,
            shell_single_quote(ack_relative_path),
            shell_single_quote(completion_relative_path),
            shell_single_quote_str(&ack_nonce)
        );
        let argv = argv(
            ["tmux", "pipe-pane", "-o", "-t", target.as_str()],
            [command.as_str()],
        );
        let redacted_argv = pipe_sink_redacted_argv(target.as_str());
        self.run_checked(argv.clone())
            .map_err(|err| redact_pipe_sink_error(err, &redacted_argv))?;
        let ready = wait_for_pipe_ack(
            root,
            ack_relative_path,
            &ack_nonce,
            identity,
            &sink,
            &redacted_argv,
            self.pipe_ready_timeout,
        )?;
        Ok(TmuxPipeCapture {
            root: root.to_path_buf(),
            transcript_relative_path: relative_path.to_path_buf(),
            completion_relative_path: completion_relative_path.to_path_buf(),
            transcript_identity: *identity,
            nonce: ack_nonce,
            helper_pid: ready.pid,
            helper_process_start_time_ticks: ready.process_start_time_ticks,
            external_helper: self.runner.pipe_sink_helper_is_external(),
        })
    }

    pub fn wait_for_pipe_capture_completion(
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
        wait_for_pipe_helper_exit(capture, deadline).map_err(|_| pipe_completion_error(&argv))?;
        remove_pipe_sink_ack_under_root(&capture.root, &capture.completion_relative_path)
            .map_err(|_| pipe_completion_error(&argv))?;
        Ok(completion)
    }

    fn validate_exact_pane(&self, metadata: &TmuxActivationMetadata) -> Result<(), TmuxError> {
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
            Err(TmuxError::CommandFailed {
                argv,
                status: output.status,
                stderr: output.stderr,
            })
        }
    }
}

#[derive(Debug, Clone)]
pub struct TmuxInputTransactionConfig {
    ledger: MachineInputLedger,
    clock: MachineInputClock,
    submit_key_count: usize,
    prompt_to_submit_delay: Duration,
    submit_key_delay: Duration,
}

impl TmuxInputTransactionConfig {
    pub fn runtime() -> Self {
        Self {
            ledger: MachineInputLedger::runtime_default(),
            clock: MachineInputClock::realtime(),
            submit_key_count: 1,
            prompt_to_submit_delay: Duration::from_secs(5),
            submit_key_delay: Duration::from_millis(250),
        }
    }

    pub fn deterministic(ledger: MachineInputLedger, timestamp_ms: u64) -> Self {
        Self {
            ledger,
            clock: MachineInputClock::fixed(timestamp_ms),
            submit_key_count: 1,
            prompt_to_submit_delay: Duration::ZERO,
            submit_key_delay: Duration::ZERO,
        }
    }

    pub fn with_ledger(mut self, ledger: MachineInputLedger) -> Self {
        self.ledger = ledger;
        self
    }

    pub fn with_submit_key_count(mut self, submit_key_count: usize) -> Self {
        self.submit_key_count = submit_key_count.max(1);
        self
    }

    pub fn with_prompt_to_submit_delay(mut self, delay: Duration) -> Self {
        self.prompt_to_submit_delay = delay;
        self
    }

    pub fn with_submit_key_delay(mut self, delay: Duration) -> Self {
        self.submit_key_delay = delay;
        self
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxInputTransaction {
    record: MachineInputRecord,
}

impl TmuxInputTransaction {
    pub fn transaction_id(&self) -> &str {
        &self.record.transaction_id
    }

    pub fn record(&self) -> &MachineInputRecord {
        &self.record
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
        }
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
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        let Some((program, args)) = argv.split_first() else {
            return Err(TmuxError::EmptyArgv);
        };

        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|err| TmuxError::Io {
                argv,
                message: err.to_string(),
            })?;
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
pub enum TmuxError {
    EmptyArgv,
    MissingSession {
        target: &'static str,
    },
    InvalidSessionName {
        reason: &'static str,
    },
    ReservedSession {
        session_id: String,
    },
    EmptyOutput {
        field: &'static str,
    },
    Io {
        argv: Vec<String>,
        message: String,
    },
    InputLedger {
        message: String,
    },
    AgentReadinessTimeout {
        pattern: String,
        timeout_ms: u64,
    },
    PaneMetadataMismatch(Box<TmuxPaneMetadataMismatch>),
    CommandFailed {
        argv: Vec<String>,
        status: i32,
        stderr: String,
    },
}

impl fmt::Display for TmuxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyArgv => write!(formatter, "empty command argv"),
            Self::MissingSession { target } => {
                write!(formatter, "tmux {target} requires session ownership")
            }
            Self::InvalidSessionName { reason } => write!(formatter, "{reason}"),
            Self::ReservedSession { session_id } => {
                write!(formatter, "tmux session named {session_id} is reserved")
            }
            Self::EmptyOutput { field } => write!(formatter, "tmux did not return {field}"),
            Self::Io { argv, message } => write!(formatter, "{}: {message}", argv.join(" ")),
            Self::InputLedger { message } => write!(formatter, "{message}"),
            Self::AgentReadinessTimeout {
                pattern,
                timeout_ms,
            } => write!(
                formatter,
                "tmux pane did not contain agent readiness pattern {pattern:?} within {timeout_ms} ms"
            ),
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
            Self::CommandFailed {
                argv,
                status,
                stderr,
            } => write!(
                formatter,
                "{} failed with status {status}: {stderr}",
                argv.join(" ")
            ),
        }
    }
}

impl Error for TmuxError {}

fn argv<const N: usize, const M: usize>(head: [&str; N], tail: [&str; M]) -> Vec<String> {
    head.into_iter().chain(tail).map(String::from).collect()
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

fn shell_single_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    shell_single_quote_str(&value)
}

fn shell_single_quote_str(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn wait_for_pipe_ack(
    root: &Path,
    ack_relative_path: &Path,
    expected_nonce: &str,
    identity: &PipeSinkIdentity,
    sink: &Path,
    argv: &[String],
    timeout: Duration,
) -> Result<PipeSinkReady, TmuxError> {
    let deadline = Instant::now() + timeout;
    loop {
        match verify_pipe_sink_ready_ack_under_root(
            root,
            ack_relative_path,
            expected_nonce,
            identity,
            sink,
        ) {
            Ok(ready) => return Ok(ready),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_err) => {
                return Err(TmuxError::Io {
                    argv: argv.to_vec(),
                    message: "pipe sink setup failed".to_string(),
                });
            }
        }
        if Instant::now() >= deadline {
            return Err(TmuxError::Io {
                argv: argv.to_vec(),
                message: format!(
                    "pipe sink did not acknowledge readiness: {}",
                    root.join(ack_relative_path).display()
                ),
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn default_pipe_completion_path(ack_relative_path: &Path) -> PathBuf {
    let file_name = ack_relative_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("pipe.ready");
    ack_relative_path.with_file_name(format!("{file_name}.complete"))
}

fn pipe_completion_error(argv: &[String]) -> TmuxError {
    TmuxError::Io {
        argv: argv.to_vec(),
        message: "pipe sink completion failed".to_string(),
    }
}

fn wait_for_pipe_helper_exit(capture: &TmuxPipeCapture, deadline: Instant) -> std::io::Result<()> {
    if !capture.external_helper {
        return Ok(());
    }
    while helper_process_matches(capture.helper_pid, capture.helper_process_start_time_ticks)? {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "pipe sink completion failed",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[cfg(all(unix, target_os = "linux"))]
fn helper_process_matches(pid: u32, expected_start_time_ticks: u64) -> std::io::Result<bool> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    let fields = stat
        .rsplit_once(')')
        .map(|(_, fields)| fields)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "pipe sink process identity verification failed",
            )
        })?;
    let actual_start_time_ticks = fields
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "pipe sink process identity verification failed",
            )
        })?
        .parse::<u64>()
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "pipe sink process identity verification failed",
            )
        })?;
    Ok(actual_start_time_ticks == expected_start_time_ticks)
}

#[cfg(not(all(unix, target_os = "linux")))]
fn helper_process_matches(_: u32, _: u64) -> std::io::Result<bool> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "durable tmux transcript capture is not supported on this platform",
    ))
}

fn pipe_sink_redacted_argv(target: &str) -> Vec<String> {
    argv(
        ["tmux", "pipe-pane", "-o", "-t", target],
        ["<pipe-sink-command-redacted>"],
    )
}

fn redact_pipe_sink_error(err: TmuxError, redacted_argv: &[String]) -> TmuxError {
    match err {
        TmuxError::Io { .. } => TmuxError::Io {
            argv: redacted_argv.to_vec(),
            message: "pipe sink setup failed".to_string(),
        },
        TmuxError::CommandFailed { status, .. } => TmuxError::CommandFailed {
            argv: redacted_argv.to_vec(),
            status,
            stderr: "pipe sink setup failed".to_string(),
        },
        other => other,
    }
}

fn pipe_ack_nonce() -> String {
    let mut bytes = [0_u8; 16];
    if File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok()
    {
        return bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    }
    let fallback = format!(
        "{}-{}-{:?}",
        std::process::id(),
        Instant::now().elapsed().as_nanos(),
        std::thread::current().id()
    );
    fallback
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
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
