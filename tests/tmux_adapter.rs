use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use humanize_plugin::adapters::lifecycle::{
    AgentLifecycleAdapter, LifecycleCleanupAction, LifecycleStatus,
};
use humanize_plugin::adapters::tmux::{
    CommandOutput, CommandRunner, SystemCommandRunner, TmuxActivationMetadata,
    TmuxActivationRequest, TmuxAdapter, TmuxError, TmuxInputTransactionConfig, TmuxPane,
    TmuxPaneIdentity, TmuxPaneMetadataMismatch, TmuxSession, TmuxWindow,
};
use humanize_plugin::input_ledger::{
    MachineInputLedger, MachineInputRecord, MachineInputStatus, machine_input_payload_hash,
};
use humanize_plugin::pipe_sink::{PipeSinkIdentity, pipe_sink_identity};

thread_local! {
    static OPEN_PIPE_ACK_FILES: RefCell<Vec<File>> = const { RefCell::new(Vec::new()) };
}

static TMUX_BIN_ENV_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Default)]
struct RecordingRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    outputs: Rc<RefCell<VecDeque<CommandOutput>>>,
}

impl RecordingRunner {
    fn with_outputs(outputs: Vec<CommandOutput>) -> Self {
        Self {
            calls: Rc::new(RefCell::new(Vec::new())),
            outputs: Rc::new(RefCell::new(outputs.into())),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        self.calls.borrow_mut().push(argv);
        Ok(self.outputs.borrow_mut().pop_front().unwrap_or_default())
    }
}

#[test]
fn only_system_runner_opts_into_external_driver_launch() {
    assert!(
        !TmuxAdapter::with_runner(RecordingRunner::default()).supports_external_driver_launch()
    );
    assert!(TmuxAdapter::with_runner(SystemCommandRunner).supports_external_driver_launch());
}

fn argv(rows: Vec<Vec<&str>>) -> Vec<Vec<String>> {
    rows.into_iter()
        .map(|row| row.into_iter().map(String::from).collect())
        .collect()
}

fn test_temp_dir(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("{name}-{}", std::process::id()));
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    path
}

#[derive(Clone)]
struct AckRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    ack_path: PathBuf,
}

#[derive(Clone, Default)]
struct RealPipeRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    child: Rc<RefCell<Option<Child>>>,
    stdin: Rc<RefCell<Option<ChildStdin>>>,
}

impl RealPipeRunner {
    fn write_pipe(&self, bytes: &[u8]) {
        self.stdin
            .borrow_mut()
            .as_mut()
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    fn close_pipe(&self) {
        self.stdin.borrow_mut().take();
        if let Some(mut child) = self.child.borrow_mut().take() {
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

impl CommandRunner for RealPipeRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        match argv.get(1).map(String::as_str) {
            Some("pipe-pane") => {
                let command = argv.get(5).cloned().unwrap_or_default();
                let mut child = Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                *self.stdin.borrow_mut() = child.stdin.take();
                *self.child.borrow_mut() = Some(child);
            }
            Some("kill-pane") => self.close_pipe(),
            _ => {}
        }
        self.calls.borrow_mut().push(argv);
        Ok(CommandOutput::success(""))
    }
}

impl AckRunner {
    fn new(ack_path: PathBuf) -> Self {
        Self {
            calls: Rc::new(RefCell::new(Vec::new())),
            ack_path,
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

#[derive(Clone)]
struct ForgedAckRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    ack_path: PathBuf,
    payload: &'static str,
}

#[derive(Clone, Default)]
struct EchoCommandFailureRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    pipe_commands: Rc<RefCell<Vec<String>>>,
}

impl EchoCommandFailureRunner {
    fn pipe_commands(&self) -> Vec<String> {
        self.pipe_commands.borrow().clone()
    }
}

impl ForgedAckRunner {
    fn new(ack_path: PathBuf, payload: &'static str) -> Self {
        Self {
            calls: Rc::new(RefCell::new(Vec::new())),
            ack_path,
            payload,
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

impl CommandRunner for ForgedAckRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        if argv.get(1).map(String::as_str) == Some("pipe-pane") {
            if let Some(parent) = self.ack_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&self.ack_path, self.payload).unwrap();
        }
        self.calls.borrow_mut().push(argv);
        Ok(CommandOutput::success(""))
    }
}

impl CommandRunner for AckRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        if argv.get(1).map(String::as_str) == Some("pipe-pane") {
            let Some(command) = argv.get(5) else {
                self.calls.borrow_mut().push(argv);
                return Ok(CommandOutput::success(""));
            };
            let root = shell_arg_after(command, "--root").unwrap();
            let relative = shell_arg_after(command, "--relative").unwrap();
            let ack_relative = shell_arg_after(command, "--ack-relative").unwrap();
            let ack_nonce = shell_arg_after(command, "--ack-nonce").unwrap();
            let dev = shell_arg_after(command, "--dev")
                .unwrap()
                .parse::<u64>()
                .unwrap();
            let ino = shell_arg_after(command, "--ino")
                .unwrap()
                .parse::<u64>()
                .unwrap();
            let transcript_path = PathBuf::from(&root).join(relative);
            let file = OpenOptions::new()
                .append(true)
                .open(&transcript_path)
                .unwrap();
            let ack_path = PathBuf::from(&root).join(ack_relative);
            if let Some(parent) = self.ack_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            assert_eq!(ack_path, self.ack_path);
            let payload = serde_json::json!({
                "nonce": ack_nonce,
                "pid": std::process::id(),
                "transcript_dev": dev,
                "transcript_ino": ino
            });
            fs::write(&self.ack_path, format!("{payload}\n")).unwrap();
            OPEN_PIPE_ACK_FILES.with(|files| files.borrow_mut().push(file));
        }
        self.calls.borrow_mut().push(argv);
        Ok(CommandOutput::success(""))
    }
}

impl CommandRunner for EchoCommandFailureRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        if argv.get(1).map(String::as_str) == Some("pipe-pane") {
            let command = argv.get(5).cloned().unwrap_or_default();
            self.pipe_commands.borrow_mut().push(command.clone());
            self.calls.borrow_mut().push(argv);
            return Ok(CommandOutput::failure(format!(
                "wrapper echoed helper command: {command}"
            )));
        }
        self.calls.borrow_mut().push(argv);
        Ok(CommandOutput::success(""))
    }
}

fn shell_arg_after(command: &str, flag: &str) -> Option<String> {
    let rest = command.split_once(flag)?.1.trim_start();
    if let Some(rest) = rest.strip_prefix('\'') {
        let value = rest.split('\'').next()?;
        return Some(value.to_string());
    }
    rest.split_whitespace().next().map(str::to_string)
}

fn assert_creation_path_rejects_session_name(session_id: &str, expected_error: &str) {
    let runner = RecordingRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let err = adapter.ensure_session(session_id).unwrap_err();
    assert_eq!(err.to_string(), expected_error);
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());

    let runner = RecordingRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let err = adapter
        .create_session_with_window_pane(session_id, "run-a", "window-a", "activation-a")
        .unwrap_err();
    assert_eq!(err.to_string(), expected_error);
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());

    let runner = RecordingRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let err = adapter
        .prepare_activation(TmuxActivationRequest::new(
            session_id,
            "run-a",
            "window-a",
            "activation-a",
        ))
        .unwrap_err();
    assert_eq!(err.to_string(), expected_error);
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
}

#[test]
fn tmux_adapter_builds_argv_for_session_window_and_pane_mapping() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success(""),
        CommandOutput::success("%1\n"),
        CommandOutput::success("%2\n"),
        CommandOutput::success(""),
        CommandOutput::success("pane text\n"),
    ]);
    let adapter = TmuxAdapter::with_runner(runner.clone());

    let session = adapter.ensure_session("host-a").unwrap();
    let window = adapter.create_window(&session, "run-a").unwrap();
    let pane = adapter
        .split_pane_for_activation(&window, "activation-a")
        .unwrap();
    adapter
        .send_keys_literal(&pane, "cargo test --quiet")
        .unwrap();
    let captured = adapter.capture_pane(&pane).unwrap();

    assert_eq!(session, TmuxSession::new("host-a"));
    assert_eq!(window.id(), "%1");
    assert_eq!(window.run_id(), "run-a");
    assert_eq!(pane.id(), "%2");
    assert_eq!(pane.activation_id(), "activation-a");
    assert_eq!(captured, "pane text\n");
    assert_eq!(
        runner.calls(),
        vec![
            vec!["tmux", "has-session", "-t", "host-a"],
            vec!["tmux", "new-session", "-d", "-s", "host-a"],
            vec![
                "tmux",
                "new-window",
                "-P",
                "-F",
                "#{window_id}",
                "-t",
                "host-a",
                "-n",
                "run-a",
            ],
            vec![
                "tmux",
                "split-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                "host-a:%1",
                "-v",
            ],
            vec![
                "tmux",
                "send-keys",
                "-t",
                "host-a:%1.%2",
                "-l",
                "cargo test --quiet",
            ],
            vec!["tmux", "capture-pane", "-p", "-t", "host-a:%1.%2"],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
    );
}

#[test]
fn tmux_wait_for_pane_text_validates_identity_and_observes_readiness() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("host-a|%7|window-a|%8\n"),
        CommandOutput::success("Use /skills to list available skills\n"),
    ]);
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let metadata =
        TmuxActivationMetadata::new("host-a", "run-a", "window-a", "%7", "activation-a", "%8");

    adapter
        .wait_for_pane_text(
            &metadata,
            "Use /skills to list available skills",
            Duration::from_millis(100),
        )
        .unwrap();

    assert_eq!(
        runner.calls(),
        argv(vec![
            vec![
                "tmux",
                "display-message",
                "-p",
                "-t",
                "host-a:%7.%8",
                "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
            ],
            vec!["tmux", "capture-pane", "-p", "-t", "host-a:%7.%8"],
        ])
    );
}

#[test]
fn tmux_send_transaction_validates_exact_pane_sends_literal_text_and_records_ledger() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success(""),
    ]);
    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(ledger.clone(), 1_000).with_submit_key_count(2),
    );
    let metadata =
        TmuxActivationMetadata::new("host-a", "run-a", "window-a", "%7", "activation-a", "%8");

    let transaction = adapter
        .send_input_transaction(&metadata, "inspect\r\nthe repo")
        .unwrap();

    let records = ledger.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].run_id, "run-a");
    assert_eq!(records[0].activation_id, "activation-a");
    assert_eq!(records[0].pane_id, "%8");
    assert_eq!(records[0].started_at_ms, 1_000);
    assert_eq!(records[0].submitted_at_ms, 1_000);
    assert_eq!(records[0].normalized_text, "inspect\nthe repo");
    assert_eq!(
        records[0].payload_hash,
        machine_input_payload_hash("inspect\r\nthe repo")
    );
    assert_ne!(
        records[0].payload_hash,
        machine_input_payload_hash(&records[0].normalized_text)
    );
    assert_eq!(records[0].submit_key_count, 2);
    assert_eq!(records[0].transaction_id, transaction.transaction_id());
    assert_eq!(records[0].status, MachineInputStatus::Started);
    assert_eq!(records[1].transaction_id, transaction.transaction_id());
    assert_eq!(records[1].status, MachineInputStatus::Submitted);
    assert!(records[0].transaction_id.starts_with("machine-input:"));
    let buffer_name = transaction.transaction_id().replace(':', "-");
    assert_eq!(
        runner.calls(),
        argv(vec![
            vec![
                "tmux",
                "display-message",
                "-p",
                "-t",
                "host-a:%7.%8",
                "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
            ],
            vec![
                "tmux",
                "set-buffer",
                "-b",
                buffer_name.as_str(),
                "--",
                "inspect\r\nthe repo",
            ],
            vec![
                "tmux",
                "paste-buffer",
                "-p",
                "-d",
                "-b",
                buffer_name.as_str(),
                "-t",
                "host-a:%7.%8",
            ],
            vec!["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"],
            vec!["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"],
        ])
    );
}

#[test]
fn tmux_send_transaction_file_ledger_keeps_started_and_submitted_records() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
    ]);
    let root = test_temp_dir("machine-input-ledger-file-transaction");
    fs::create_dir_all(&root).unwrap();
    let ledger_path = root.join("machine-inputs.jsonl");
    let ledger = MachineInputLedger::at_path(&ledger_path);
    let adapter = TmuxAdapter::with_runner(runner)
        .with_input_transaction_config(TmuxInputTransactionConfig::deterministic(ledger, 1_000));
    let metadata =
        TmuxActivationMetadata::new("host-a", "run-a", "window-a", "%7", "activation-a", "%8");

    let transaction = adapter
        .send_input_transaction(&metadata, "inspect the repo")
        .unwrap();

    let records = fs::read_to_string(&ledger_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<MachineInputRecord>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].transaction_id, transaction.transaction_id());
    assert_eq!(records[0].status, MachineInputStatus::Started);
    assert_eq!(records[1].transaction_id, transaction.transaction_id());
    assert_eq!(records[1].status, MachineInputStatus::Submitted);
}

#[test]
fn tmux_send_transaction_records_failed_status_when_enter_send_fails() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::failure("cannot send enter"),
    ]);
    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(ledger.clone(), 1_000),
    );
    let metadata =
        TmuxActivationMetadata::new("host-a", "run-a", "window-a", "%7", "activation-a", "%8");

    let err = adapter
        .send_input_transaction(&metadata, "inspect the repo")
        .unwrap_err();

    assert!(matches!(err, TmuxError::CommandFailed { .. }));
    let records = ledger.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].status, MachineInputStatus::Started);
    assert_eq!(records[1].status, MachineInputStatus::Failed);
    assert_eq!(records[0].transaction_id, records[1].transaction_id);
    assert_eq!(records[1].normalized_text, "inspect the repo");
}

#[test]
fn system_tmux_adapter_uses_explicit_real_tmux_binary_and_ledgers_input() {
    let _guard = TMUX_BIN_ENV_LOCK.lock().unwrap();
    let root = test_temp_dir("tmux-adapter-real-bin");
    fs::create_dir_all(&root).unwrap();
    let fake_tmux = root.join("real-tmux");
    let calls = root.join("tmux-calls.txt");
    fs::write(
        &fake_tmux,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = display-message ]; then printf 'host-a\\t%%7\\tflow-a\\t%%8\\n'; fi\n",
            calls.display()
        ),
    )
    .unwrap();
    make_executable(&fake_tmux);
    let prior = std::env::var_os("HUMANIZE_TMUX_BIN");
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
    }

    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(SystemCommandRunner).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(ledger.clone(), 1_700_000_000_000)
            .with_prompt_to_submit_delay(Duration::from_millis(0)),
    );
    let metadata =
        TmuxActivationMetadata::new("host-a", "run-real-bin", "flow-a", "%7", "root", "%8");

    let transaction = adapter
        .send_input_transaction(&metadata, "inspect the repo")
        .unwrap();

    restore_env("HUMANIZE_TMUX_BIN", prior);
    assert_eq!(transaction.record().status, MachineInputStatus::Submitted);
    let calls = fs::read_to_string(calls).unwrap();
    assert!(calls.contains("send-keys -t host-a:%7.%8 -l inspect the repo"));
    assert!(calls.contains("send-keys -t host-a:%7.%8 Enter"));
    assert_eq!(ledger.records().len(), 2);
    assert_eq!(ledger.records()[1].normalized_text, "inspect the repo");
}

#[test]
fn tmux_send_transaction_does_not_send_when_initial_ledger_record_fails() {
    let runner =
        RecordingRunner::with_outputs(vec![CommandOutput::success("host-a\t%7\twindow-a\t%8\n")]);
    let ledger_path = test_temp_dir("machine-input-ledger-directory");
    fs::create_dir_all(&ledger_path).unwrap();
    let ledger = MachineInputLedger::at_path(&ledger_path);
    let adapter = TmuxAdapter::with_runner(runner.clone())
        .with_input_transaction_config(TmuxInputTransactionConfig::deterministic(ledger, 1_000));
    let metadata =
        TmuxActivationMetadata::new("host-a", "run-a", "window-a", "%7", "activation-a", "%8");

    let err = adapter
        .send_input_transaction(&metadata, "inspect the repo")
        .unwrap_err();

    assert!(matches!(err, TmuxError::InputLedger { .. }));
    assert_eq!(
        runner.calls(),
        argv(vec![vec![
            "tmux",
            "display-message",
            "-p",
            "-t",
            "host-a:%7.%8",
            "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
        ]])
    );
    fs::remove_dir_all(&ledger_path).unwrap();
}

#[test]
fn tmux_send_transaction_rejects_pane_metadata_mismatch_before_send() {
    let runner =
        RecordingRunner::with_outputs(vec![CommandOutput::success("host-b\t%7\twindow-a\t%8\n")]);
    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(ledger.clone(), 1_000),
    );
    let metadata =
        TmuxActivationMetadata::new("host-a", "run-a", "window-a", "%7", "activation-a", "%8");

    let err = adapter
        .send_input_transaction(&metadata, "inspect the repo")
        .unwrap_err();

    assert_eq!(
        err,
        TmuxError::PaneMetadataMismatch(Box::new(TmuxPaneMetadataMismatch::new(
            TmuxPaneIdentity::new("host-a", "%7", "window-a", "%8"),
            TmuxPaneIdentity::new("host-b", "%7", "window-a", "%8"),
        )))
    );
    assert_eq!(ledger.records(), Vec::new());
    assert_eq!(
        runner.calls(),
        argv(vec![vec![
            "tmux",
            "display-message",
            "-p",
            "-t",
            "host-a:%7.%8",
            "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
        ]])
    );
}

#[test]
fn tmux_send_transaction_rejects_window_name_mismatch_before_send() {
    let runner = RecordingRunner::with_outputs(vec![CommandOutput::success(
        "host-a\t%7\tother-window\t%8\n",
    )]);
    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(ledger.clone(), 1_000),
    );
    let metadata =
        TmuxActivationMetadata::new("host-a", "run-a", "window-a", "%7", "activation-a", "%8");

    let err = adapter
        .send_input_transaction(&metadata, "inspect the repo")
        .unwrap_err();

    assert_eq!(
        err,
        TmuxError::PaneMetadataMismatch(Box::new(TmuxPaneMetadataMismatch::new(
            TmuxPaneIdentity::new("host-a", "%7", "window-a", "%8"),
            TmuxPaneIdentity::new("host-a", "%7", "other-window", "%8"),
        )))
    );
    assert_eq!(ledger.records(), Vec::new());
    assert_eq!(
        runner.calls(),
        argv(vec![vec![
            "tmux",
            "display-message",
            "-p",
            "-t",
            "host-a:%7.%8",
            "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
        ]])
    );
}

#[test]
fn tmux_creation_rejects_empty_session_name_before_runner_calls() {
    assert_creation_path_rejects_session_name("", "tmux session name must not be empty");
}

#[test]
fn tmux_creation_rejects_ambiguous_session_names_before_runner_calls() {
    for session_id in ["host:a", "host.a"] {
        assert_creation_path_rejects_session_name(
            session_id,
            "tmux session name must not contain tmux target delimiters ':' or '.'",
        );
    }
}

#[test]
fn tmux_adapter_creates_session_with_initial_window_and_pane() {
    let runner = RecordingRunner::with_outputs(vec![CommandOutput::success("%7\t%8\n")]);
    let adapter = TmuxAdapter::with_runner(runner.clone());

    let (session, window, pane) = adapter
        .create_session_with_window_pane("host-a", "run-a", "window-a", "activation-a")
        .unwrap();

    assert_eq!(session, TmuxSession::new("host-a"));
    assert_eq!(window.session_id(), "host-a");
    assert_eq!(window.run_id(), "run-a");
    assert_eq!(window.name(), "window-a");
    assert_eq!(window.id(), "%7");
    assert_eq!(pane.session_id(), "host-a");
    assert_eq!(pane.window_id(), "%7");
    assert_eq!(pane.activation_id(), "activation-a");
    assert_eq!(pane.id(), "%8");
    assert_eq!(
        runner.calls(),
        argv(vec![vec![
            "tmux",
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{window_id}|#{pane_id}",
            "-s",
            "host-a",
            "-n",
            "window-a",
        ]])
    );
}

#[test]
fn tmux_adapter_creates_window_with_initial_pane() {
    let runner = RecordingRunner::with_outputs(vec![CommandOutput::success("%9\t%10\n")]);
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let session = TmuxSession::new("host-a");

    let (window, pane) = adapter
        .create_window_named_with_pane(&session, "run-b", "window-b", "activation-b")
        .unwrap();

    assert_eq!(window.session_id(), "host-a");
    assert_eq!(window.run_id(), "run-b");
    assert_eq!(window.name(), "window-b");
    assert_eq!(window.id(), "%9");
    assert_eq!(pane.session_id(), "host-a");
    assert_eq!(pane.window_id(), "%9");
    assert_eq!(pane.activation_id(), "activation-b");
    assert_eq!(pane.id(), "%10");
    assert_eq!(
        runner.calls(),
        argv(vec![vec![
            "tmux",
            "new-window",
            "-P",
            "-F",
            "#{window_id}|#{pane_id}",
            "-t",
            "host-a",
            "-n",
            "window-b",
        ]])
    );
}

#[test]
fn ensure_session_skips_creation_when_host_session_exists() {
    let runner = RecordingRunner::with_outputs(vec![CommandOutput::success("")]);
    let adapter = TmuxAdapter::with_runner(runner.clone());

    let session = adapter.ensure_session("host-a").unwrap();

    assert_eq!(session.id(), "host-a");
    assert_eq!(
        runner.calls(),
        vec![vec!["tmux", "has-session", "-t", "host-a"]]
            .into_iter()
            .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );
}

#[test]
fn tmux_adapter_builds_argv_for_pane_window_and_session_cleanup() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success(""),
    ]);
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let session = TmuxSession::new("host-a");
    let window = TmuxWindow::new_named("host-a", "run-a", "window-a", "%1");
    let pane = TmuxPane::new_in_session("host-a", "%1", "activation-a", "%2");

    adapter.kill_pane(&pane).unwrap();
    adapter.kill_window(&window).unwrap();
    adapter.kill_session(&session).unwrap();

    assert_eq!(
        runner.calls(),
        argv(vec![
            vec!["tmux", "kill-pane", "-t", "host-a:%1.%2"],
            vec!["tmux", "kill-window", "-t", "host-a:%1"],
            vec!["tmux", "kill-session", "-t", "host-a"],
        ])
    );
}

#[test]
fn tmux_adapter_starts_durable_pipe_for_owned_pane() {
    let root = test_temp_dir("tmux-pipe-start");
    let transcript_relative = "activations/root/transcript.pipe.log";
    let transcript_path = root.join(transcript_relative);
    fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
    fs::write(&transcript_path, "").unwrap();
    let identity = pipe_sink_identity(&transcript_path).unwrap();
    let ack_relative = "activations/root/pipe.ready";
    let runner = AckRunner::new(root.join(ack_relative));
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let pane = TmuxPane::new_in_session("host-a", "%1", "activation-a", "%2");

    adapter
        .start_pipe_capture(&pane, &root, transcript_relative, &identity, ack_relative)
        .unwrap();

    let calls = runner.calls();
    assert_eq!(
        &calls[0][..5],
        ["tmux", "pipe-pane", "-o", "-t", "host-a:%1.%2"]
    );
    assert!(calls[0][5].contains("--pipe-sink"));
    assert!(calls[0][5].contains("--root"));
    assert!(calls[0][5].contains("--relative"));
    assert!(calls[0][5].contains(transcript_relative));
    assert!(!calls[0][5].contains(transcript_path.to_string_lossy().as_ref()));
    assert!(!calls[0][5].contains("cat >>"));
}

#[cfg(all(unix, not(target_os = "linux")))]
#[test]
fn tmux_adapter_rejects_durable_pipe_capture_before_helper_launch() {
    let root = test_temp_dir("tmux-pipe-unsupported-platform");
    let transcript_relative = "activations/root/transcript.pipe.log";
    let transcript_path = root.join(transcript_relative);
    fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
    fs::write(&transcript_path, "").unwrap();
    let identity = pipe_sink_identity(&transcript_path).unwrap();
    let runner = RecordingRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let pane = TmuxPane::new_in_session("host-a", "%1", "activation-a", "%2");

    let error = adapter
        .start_pipe_capture(
            &pane,
            &root,
            transcript_relative,
            &identity,
            "activations/root/pipe.ready",
        )
        .unwrap_err();

    assert!(error.to_string().contains("not supported"));
    assert!(runner.calls().is_empty());
}

#[cfg(all(unix, target_os = "linux"))]
#[test]
fn tmux_adapter_waits_for_durable_pipe_completion_after_producer_eof() {
    let root = test_temp_dir("tmux-pipe-completion");
    let transcript_relative = "activations/root/transcript.pipe.log";
    let transcript_path = root.join(transcript_relative);
    fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
    fs::write(&transcript_path, "prefix\n").unwrap();
    let identity = pipe_sink_identity(&transcript_path).unwrap();
    let runner = RealPipeRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone())
        .with_pipe_sink_executable(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .with_pipe_capture_timeouts(Duration::from_secs(2), Duration::from_secs(2));
    let pane = TmuxPane::new_in_session("host-a", "%1", "activation-a", "%2");

    let capture = adapter
        .start_pipe_capture_with_completion(
            &pane,
            &root,
            transcript_relative,
            &identity,
            "activations/root/pipe.ready",
            "activations/root/pipe.complete",
        )
        .unwrap();
    runner.write_pipe(b"body\ntrailing bytes\n");
    adapter.kill_pane(&pane).unwrap();
    let completion = adapter.wait_for_pipe_capture_completion(&capture).unwrap();

    assert_eq!(completion.bytes_appended, 20);
    assert_eq!(completion.transcript_len, 27);
    assert_eq!(
        fs::read_to_string(transcript_path).unwrap(),
        "prefix\nbody\ntrailing bytes\n"
    );
}

#[cfg(all(unix, target_os = "linux"))]
#[test]
fn tmux_adapter_rejects_pipe_completion_before_producer_eof() {
    let root = test_temp_dir("tmux-pipe-completion-timeout");
    let transcript_relative = "activations/root/transcript.pipe.log";
    let transcript_path = root.join(transcript_relative);
    fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
    fs::write(&transcript_path, "").unwrap();
    let identity = pipe_sink_identity(&transcript_path).unwrap();
    let runner = RealPipeRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone())
        .with_pipe_sink_executable(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .with_pipe_capture_timeouts(Duration::from_secs(2), Duration::from_millis(50));
    let pane = TmuxPane::new_in_session("host-a", "%1", "activation-a", "%2");

    let capture = adapter
        .start_pipe_capture_with_completion(
            &pane,
            &root,
            transcript_relative,
            &identity,
            "activations/root/pipe.ready",
            "activations/root/pipe.complete",
        )
        .unwrap();
    runner.write_pipe(b"not closed\n");
    let err = adapter
        .wait_for_pipe_capture_completion(&capture)
        .expect_err("open producer must not be reported complete");
    runner.close_pipe();

    let message = err.to_string();
    assert!(message.contains("operation=pipe-sink"), "{message}");
    assert!(message.contains("command_hash=sha256:"), "{message}");
    assert!(message.contains("detail_hash=sha256:"), "{message}");
}

#[cfg(all(unix, target_os = "linux"))]
#[test]
fn tmux_adapter_real_tmux_pipe_drains_before_completion() {
    let _guard = TMUX_BIN_ENV_LOCK.lock().unwrap();
    let root = test_temp_dir("tmux-real-pipe-completion");
    let transcript_relative = "activations/root/transcript.pipe.log";
    let transcript_path = root.join(transcript_relative);
    fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
    fs::write(&transcript_path, "").unwrap();
    let identity = pipe_sink_identity(&transcript_path).unwrap();
    let session_id = format!(
        "humanize-pipe-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _cleanup = TmuxSessionCleanup(session_id.clone());
    let adapter = TmuxAdapter::with_runner(SystemCommandRunner)
        .with_pipe_sink_executable(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .with_pipe_capture_timeouts(Duration::from_secs(2), Duration::from_secs(3));
    let (_, _, pane) = adapter
        .create_session_with_window_pane(&session_id, "run-real-pipe", "flow-a", "root")
        .unwrap();
    let capture = adapter
        .start_pipe_capture_with_completion(
            &pane,
            &root,
            transcript_relative,
            &identity,
            "activations/root/pipe.ready",
            "activations/root/pipe.complete",
        )
        .unwrap();

    adapter
        .send_keys_literal(&pane, "printf 'durable trailing marker\\n'")
        .unwrap();
    adapter.send_key(&pane, "Enter").unwrap();
    thread::sleep(Duration::from_millis(150));
    let final_capture = adapter.capture_pane(&pane).unwrap();
    adapter.kill_pane(&pane).unwrap();
    let completion = adapter.wait_for_pipe_capture_completion(&capture).unwrap();

    assert!(final_capture.contains("durable trailing marker"));
    assert!(
        fs::read_to_string(&transcript_path)
            .unwrap()
            .contains("durable trailing marker")
    );
    assert!(completion.bytes_appended > 0);
}

#[cfg(all(unix, target_os = "linux"))]
struct TmuxSessionCleanup(String);

#[cfg(all(unix, target_os = "linux"))]
impl Drop for TmuxSessionCleanup {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.0])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[test]
fn tmux_adapter_rejects_forged_pipe_acknowledgement() {
    let root = test_temp_dir("tmux-pipe-forged-ack");
    let transcript_relative = "activations/root/transcript.pipe.log";
    let transcript_path = root.join(transcript_relative);
    fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
    fs::write(&transcript_path, "").unwrap();
    let ack_relative = "activations/root/pipe.ready";
    let runner = ForgedAckRunner::new(root.join(ack_relative), "ready\n");
    let adapter = TmuxAdapter::with_runner(runner);
    let pane = TmuxPane::new_in_session("host-a", "%1", "activation-a", "%2");

    let err = adapter
        .start_pipe_capture(
            &pane,
            &root,
            transcript_relative,
            &PipeSinkIdentity {
                dev: 7,
                ino: 8,
                uid: 9,
                mode: 0o600,
                nlink: 1,
            },
            ack_relative,
        )
        .expect_err("forged acknowledgement must be rejected");

    let message = err.to_string();
    assert!(message.contains("operation=pipe-pane"), "{message}");
    assert!(message.contains("command_hash=sha256:"), "{message}");
    assert!(message.contains("detail_hash=sha256:"), "{message}");
}

#[test]
fn tmux_adapter_redacts_pipe_ack_command_and_nonce_from_errors() {
    let root = test_temp_dir("tmux-pipe-ack-redaction");
    let transcript_relative = "activations/root/transcript.pipe.log";
    let transcript_path = root.join(transcript_relative);
    fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
    fs::write(&transcript_path, "").unwrap();
    let identity = pipe_sink_identity(&transcript_path).unwrap();
    let ack_relative = "activations/root/pipe.ready";
    let runner = ForgedAckRunner::new(root.join(ack_relative), "not-json\n");
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let pane = TmuxPane::new_in_session("host-a", "%1", "activation-a", "%2");

    let err = adapter
        .start_pipe_capture(&pane, &root, transcript_relative, &identity, ack_relative)
        .expect_err("invalid acknowledgement must be rejected");

    let command = runner.calls()[0][5].clone();
    let nonce = shell_arg_after(&command, "--ack-nonce").unwrap();
    let message = err.to_string();
    assert!(!message.contains("--ack-nonce"));
    assert!(!message.contains(&nonce));
    assert!(!message.contains(&command));
}

#[test]
fn tmux_adapter_redacts_pipe_command_failure_stderr() {
    let root = test_temp_dir("tmux-pipe-failure-redaction");
    let transcript_relative = "activations/root/transcript.pipe.log";
    let transcript_path = root.join(transcript_relative);
    fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
    fs::write(&transcript_path, "").unwrap();
    let identity = pipe_sink_identity(&transcript_path).unwrap();
    let ack_relative = "activations/root/pipe.ready";
    let runner = EchoCommandFailureRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let pane = TmuxPane::new_in_session("host-a", "%1", "activation-a", "%2");

    let err = adapter
        .start_pipe_capture(&pane, &root, transcript_relative, &identity, ack_relative)
        .expect_err("pipe-pane command failure must be rejected");

    let command = runner.pipe_commands().pop().unwrap();
    let nonce = shell_arg_after(&command, "--ack-nonce").unwrap();
    let message = err.to_string();
    assert!(!message.contains("--ack-nonce"));
    assert!(!message.contains(&nonce));
    assert!(!message.contains(&command));
    assert!(message.contains("operation=pipe-pane"), "{message}");
    assert!(message.contains("command_hash=sha256:"), "{message}");
    assert!(message.contains("detail_hash=sha256:"), "{message}");
}

#[test]
fn tmux_adapter_captures_pane_before_pane_kill() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("final transcript\n"),
        CommandOutput::success(""),
    ]);
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let pane = TmuxPane::new_in_session("host-a", "%1", "activation-a", "%2");

    assert_eq!(adapter.capture_pane(&pane).unwrap(), "final transcript\n");
    adapter.kill_pane(&pane).unwrap();

    assert_eq!(
        runner.calls(),
        argv(vec![
            vec!["tmux", "capture-pane", "-p", "-t", "host-a:%1.%2"],
            vec!["tmux", "kill-pane", "-t", "host-a:%1.%2"],
        ])
    );
}

#[test]
fn tmux_adapter_rejects_unowned_pane_cleanup_before_runner_calls() {
    let runner = RecordingRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let pane = TmuxPane::new("%1", "activation-a", "%2");

    let err = adapter.kill_pane(&pane).expect_err("pane must be owned");

    assert_eq!(err.to_string(), "tmux pane requires session ownership");
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
}

#[test]
fn tmux_adapter_rejects_unowned_pane_send_and_capture_before_runner_calls() {
    let runner = RecordingRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let pane = TmuxPane::new("%1", "activation-a", "%2");

    let send_err = adapter
        .send_keys_literal(&pane, "cargo test")
        .expect_err("send must require pane ownership");
    let capture_err = adapter
        .capture_pane(&pane)
        .expect_err("capture must require pane ownership");

    assert_eq!(send_err, TmuxError::MissingSession { target: "pane" });
    assert_eq!(capture_err, TmuxError::MissingSession { target: "pane" });
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
}

#[test]
fn tmux_adapter_rejects_reserved_dev_cleanup_before_runner_calls() {
    let runner = RecordingRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let session = TmuxSession::new("dev");
    let window = TmuxWindow::new_named("dev", "run-a", "window-a", "%1");
    let pane = TmuxPane::new_in_session("dev", "%1", "activation-a", "%2");

    assert_eq!(
        adapter.kill_pane(&pane).unwrap_err(),
        TmuxError::ReservedSession {
            session_id: "dev".to_string()
        }
    );
    assert_eq!(
        adapter.kill_window(&window).unwrap_err(),
        TmuxError::ReservedSession {
            session_id: "dev".to_string()
        }
    );
    assert_eq!(
        adapter.kill_session(&session).unwrap_err(),
        TmuxError::ReservedSession {
            session_id: "dev".to_string()
        }
    );
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
}

#[test]
fn tmux_adapter_rejects_reserved_dev_send_and_capture_before_runner_calls() {
    let runner = RecordingRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let pane = TmuxPane::new_in_session("dev", "%1", "activation-a", "%2");

    assert_eq!(
        adapter.send_keys_literal(&pane, "cargo test").unwrap_err(),
        TmuxError::ReservedSession {
            session_id: "dev".to_string()
        }
    );
    assert_eq!(
        adapter.capture_pane(&pane).unwrap_err(),
        TmuxError::ReservedSession {
            session_id: "dev".to_string()
        }
    );
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
}

#[test]
fn tmux_cleanup_reports_runner_failures_without_real_tmux() {
    let runner = RecordingRunner::with_outputs(vec![CommandOutput::failure("pane missing")]);
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let pane = TmuxPane::new_in_session("host-a", "%1", "activation-a", "%2");

    let err = adapter.kill_pane(&pane).unwrap_err();

    assert_eq!(
        err,
        TmuxError::command_failed(
            &vec!["tmux", "kill-pane", "-t", "host-a:%1.%2"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            1,
            "pane missing",
        )
    );
    assert_eq!(
        runner.calls(),
        argv(vec![vec!["tmux", "kill-pane", "-t", "host-a:%1.%2"]])
    );
}

#[test]
fn tmux_lifecycle_capabilities_advertise_tmux_observation_without_mcp_events() {
    let adapter = TmuxAdapter::with_runner(RecordingRunner::default());

    let capabilities = adapter.capabilities();

    assert!(capabilities.interactive_pane);
    assert!(!capabilities.mcp_tools);
    assert!(!capabilities.mcp_artifact_delivery);
    assert!(!capabilities.stop_hook);
    assert!(!capabilities.tool_events);
    assert!(!capabilities.permission_events);
    assert!(!capabilities.notification_events);
    assert!(!capabilities.jsonl_events);
    assert!(!capabilities.session_resume);
    assert!(capabilities.process_exit);
    assert!(capabilities.tmux_observation);
    assert!(!capabilities.structured_output);
}

#[test]
fn tmux_lifecycle_allocates_starts_prompts_observes_and_releases_satisfied_activation() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\n"),
        CommandOutput::success("%8\n"),
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success("ready\n"),
        CommandOutput::success(""),
    ]);
    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(ledger.clone(), 2_000),
    );
    let request = TmuxActivationRequest::new("host-a", "run-a", "window-a", "activation-a");

    let activation = adapter.prepare_activation(request).unwrap();
    let handle = adapter
        .start_agent(&activation, "humanize-plugin-mcp --stdio")
        .unwrap();
    adapter.send_prompt(&handle, "inspect the repo").unwrap();
    let observation = adapter.observe_lifecycle(&handle).unwrap();
    let cleanup = adapter
        .cleanup_activation(&handle, LifecycleStatus::ContractSatisfied)
        .unwrap();

    assert_eq!(activation.metadata().session_id(), "host-a");
    assert_eq!(activation.metadata().window_id(), "%7");
    assert_eq!(activation.metadata().pane_id(), "%8");
    assert_eq!(activation.metadata().run_id(), "run-a");
    assert_eq!(activation.metadata().window_name(), "window-a");
    assert_eq!(activation.metadata().activation_id(), "activation-a");
    assert_eq!(handle.pane().id(), "%8");
    assert_eq!(observation.captured_text(), "ready\n");
    assert_eq!(cleanup.action(), LifecycleCleanupAction::KillPane);
    let records = ledger.records();
    assert_eq!(records.len(), 4);
    assert_eq!(records[0].run_id, "run-a");
    assert_eq!(records[0].activation_id, "activation-a");
    assert_eq!(records[0].normalized_text, "humanize-plugin-mcp --stdio");
    assert_eq!(records[0].submit_key_count, 1);
    assert_eq!(records[0].status, MachineInputStatus::Started);
    assert_eq!(records[1].status, MachineInputStatus::Submitted);
    assert_eq!(records[2].run_id, "run-a");
    assert_eq!(records[2].activation_id, "activation-a");
    assert_eq!(records[2].normalized_text, "inspect the repo");
    assert_eq!(records[2].submit_key_count, 2);
    assert_eq!(records[2].status, MachineInputStatus::Started);
    assert_eq!(records[3].status, MachineInputStatus::Submitted);
    assert_eq!(
        runner.calls(),
        argv(vec![
            vec!["tmux", "has-session", "-t", "host-a"],
            vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
                "-s",
                "host-a",
                "-n",
                "window-a",
            ],
            vec![
                "tmux",
                "split-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                "host-a:%7",
                "-v",
            ],
            vec![
                "tmux",
                "display-message",
                "-p",
                "-t",
                "host-a:%7.%8",
                "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
            ],
            vec![
                "tmux",
                "send-keys",
                "-t",
                "host-a:%7.%8",
                "-l",
                "humanize-plugin-mcp --stdio",
            ],
            vec!["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"],
            vec![
                "tmux",
                "display-message",
                "-p",
                "-t",
                "host-a:%7.%8",
                "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
            ],
            vec![
                "tmux",
                "send-keys",
                "-t",
                "host-a:%7.%8",
                "-l",
                "inspect the repo",
            ],
            vec!["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"],
            vec!["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"],
            vec!["tmux", "capture-pane", "-p", "-t", "host-a:%7.%8"],
            vec!["tmux", "kill-pane", "-t", "host-a:%7.%8"],
        ])
    );
}

#[test]
fn tmux_lifecycle_rejects_reserved_dev_prepare_before_runner_calls() {
    let runner = RecordingRunner::default();
    let adapter = TmuxAdapter::with_runner(runner.clone());

    let err = adapter
        .prepare_activation(TmuxActivationRequest::new(
            "dev",
            "run-a",
            "window-a",
            "activation-a",
        ))
        .unwrap_err();

    assert_eq!(
        err,
        TmuxError::ReservedSession {
            session_id: "dev".to_string()
        }
    );
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
}

#[test]
fn tmux_lifecycle_reuses_workflow_window_for_repeated_activations() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\n"),
        CommandOutput::success("%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("window-a\t%7\n"),
        CommandOutput::success("%9\n"),
    ]);
    let adapter = TmuxAdapter::with_runner(runner.clone());

    let first = adapter
        .prepare_activation(TmuxActivationRequest::new(
            "host-a",
            "run-a",
            "window-a",
            "activation-a",
        ))
        .unwrap();
    let second = adapter
        .prepare_activation(TmuxActivationRequest::new(
            "host-a",
            "run-a",
            "window-a",
            "activation-b",
        ))
        .unwrap();

    assert_eq!(first.metadata().window_id(), "%7");
    assert_eq!(first.metadata().pane_id(), "%8");
    assert_eq!(second.metadata().window_id(), "%7");
    assert_eq!(second.metadata().pane_id(), "%9");
    assert_eq!(
        runner.calls(),
        argv(vec![
            vec!["tmux", "has-session", "-t", "host-a"],
            vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
                "-s",
                "host-a",
                "-n",
                "window-a",
            ],
            vec![
                "tmux",
                "split-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                "host-a:%7",
                "-v",
            ],
            vec!["tmux", "has-session", "-t", "host-a"],
            vec![
                "tmux",
                "list-windows",
                "-t",
                "host-a",
                "-F",
                "#{window_name}|#{window_id}",
            ],
            vec![
                "tmux",
                "split-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                "host-a:%7",
                "-v",
            ],
        ])
    );
}

#[test]
fn tmux_lifecycle_preserves_blocked_or_failed_activation_by_default() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\n"),
        CommandOutput::success("%8\n"),
        CommandOutput::failure("missing session"),
        CommandOutput::success("%9\n"),
        CommandOutput::success("%10\n"),
    ]);
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let blocked = adapter
        .prepare_activation(TmuxActivationRequest::new(
            "host-a",
            "run-a",
            "window-a",
            "activation-a",
        ))
        .unwrap();
    let failed = adapter
        .prepare_activation(TmuxActivationRequest::new(
            "host-b",
            "run-b",
            "window-b",
            "activation-b",
        ))
        .unwrap();
    let calls_after_allocation = runner.calls();

    let blocked_cleanup = adapter
        .cleanup_activation(&blocked.into_handle(), LifecycleStatus::Blocked)
        .unwrap();
    let failed_cleanup = adapter
        .cleanup_activation(&failed.into_handle(), LifecycleStatus::Failed)
        .unwrap();

    assert_eq!(
        blocked_cleanup.action(),
        LifecycleCleanupAction::PreservePane
    );
    assert_eq!(
        failed_cleanup.action(),
        LifecycleCleanupAction::PreservePane
    );
    assert_eq!(runner.calls(), calls_after_allocation);
}

fn restore_env(name: &str, prior: Option<OsString>) {
    unsafe {
        match prior {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
