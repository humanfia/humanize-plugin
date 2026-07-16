use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

use humanize_plugin::adapters::tmux::{
    CommandOutput, CommandRunner, SystemCommandRunner, TmuxActivationMetadata, TmuxAdapter,
    TmuxError, TmuxInputTransactionConfig, TmuxPaneIdentity, TmuxPaneMetadataMismatch,
};
use humanize_plugin::input_ledger::{
    MachineInputLedger, MachineInputRecord, MachineInputStatus, machine_input_payload_hash,
};

static TMUX_BIN_ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_tmux_bin_env() -> std::sync::MutexGuard<'static, ()> {
    TMUX_BIN_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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

#[derive(Clone, Default)]
struct BufferTrackingRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    buffers: Rc<RefCell<BTreeMap<String, String>>>,
    outputs: Rc<RefCell<VecDeque<CommandOutput>>>,
}

impl BufferTrackingRunner {
    fn with_outputs(outputs: Vec<CommandOutput>) -> Self {
        Self {
            calls: Rc::new(RefCell::new(Vec::new())),
            buffers: Rc::new(RefCell::new(BTreeMap::new())),
            outputs: Rc::new(RefCell::new(outputs.into())),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }

    fn contains_buffer(&self, buffer_name: &str) -> bool {
        self.buffers.borrow().contains_key(buffer_name)
    }
}

impl CommandRunner for BufferTrackingRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        self.calls.borrow_mut().push(argv.clone());
        let output = self.outputs.borrow_mut().pop_front().unwrap_or_default();
        if !output.is_success() {
            return Ok(output);
        }

        let buffer_name = argv
            .windows(2)
            .find_map(|window| (window[0] == "-b").then(|| window[1].clone()));
        match argv.get(1).map(String::as_str) {
            Some("set-buffer") => {
                let text = argv
                    .iter()
                    .position(|argument| argument == "--")
                    .and_then(|index| argv.get(index + 1))
                    .cloned()
                    .expect("set-buffer payload");
                self.buffers
                    .borrow_mut()
                    .insert(buffer_name.expect("set-buffer name"), text);
            }
            Some("paste-buffer") | Some("delete-buffer") => {
                self.buffers
                    .borrow_mut()
                    .remove(&buffer_name.expect("buffer name"));
            }
            _ => {}
        }
        Ok(output)
    }
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

#[test]
fn tmux_send_transaction_validates_exact_pane_sends_literal_text_and_records_ledger() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
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
                "display-message",
                "-p",
                "-t",
                "host-a:%7.%8",
                "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
            ],
            vec![
                "tmux",
                "paste-buffer",
                "-p",
                "-r",
                "-d",
                "-b",
                buffer_name.as_str(),
                "-t",
                "host-a:%7.%8",
            ],
            vec![
                "tmux",
                "display-message",
                "-p",
                "-t",
                "host-a:%7.%8",
                "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
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
            vec!["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"],
        ])
    );
}

#[test]
fn tmux_send_transaction_file_ledger_keeps_started_and_submitted_records() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
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
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
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
fn tmux_transaction_deletes_staged_buffer_when_exact_pane_validation_fails() {
    let runner = BufferTrackingRunner::with_outputs(vec![
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::failure("target pane disappeared"),
        CommandOutput::success(""),
    ]);
    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(ledger.clone(), 1_000),
    );
    let metadata =
        TmuxActivationMetadata::new("host-a", "run-a", "window-a", "%7", "activation-a", "%8");

    let error = adapter
        .send_input_transaction(&metadata, "sensitive prompt")
        .unwrap_err();

    assert!(error.to_string().contains("display-message"));
    let records = ledger.records();
    let buffer_name = records[0].transaction_id.replace(':', "-");
    assert_eq!(
        records
            .iter()
            .map(|record| record.status.clone())
            .collect::<Vec<_>>(),
        vec![MachineInputStatus::Started, MachineInputStatus::Failed]
    );
    assert!(runner.calls().contains(
        &argv(vec![vec![
            "tmux",
            "delete-buffer",
            "-b",
            buffer_name.as_str(),
        ]])[0]
    ));
    assert!(!runner.contains_buffer(&buffer_name));
}

#[test]
fn tmux_transaction_deletes_staged_buffer_when_paste_fails() {
    let runner = BufferTrackingRunner::with_outputs(vec![
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::failure("paste failed"),
        CommandOutput::success(""),
    ]);
    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(ledger.clone(), 1_000),
    );
    let metadata =
        TmuxActivationMetadata::new("host-a", "run-a", "window-a", "%7", "activation-a", "%8");

    let error = adapter
        .send_input_transaction(&metadata, "sensitive prompt")
        .unwrap_err();

    assert!(error.to_string().contains("paste-buffer"));
    let records = ledger.records();
    let buffer_name = records[0].transaction_id.replace(':', "-");
    assert_eq!(
        records
            .iter()
            .map(|record| record.status.clone())
            .collect::<Vec<_>>(),
        vec![MachineInputStatus::Started, MachineInputStatus::Failed]
    );
    assert!(runner.calls().contains(
        &argv(vec![vec![
            "tmux",
            "delete-buffer",
            "-b",
            buffer_name.as_str(),
        ]])[0]
    ));
    assert!(!runner.contains_buffer(&buffer_name));
}

#[test]
fn tmux_transaction_preserves_paste_failure_when_buffer_cleanup_fails() {
    let runner = BufferTrackingRunner::with_outputs(vec![
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\twindow-a\t%8\n"),
        CommandOutput::failure("paste failed"),
        CommandOutput::failure("buffer cleanup failed"),
    ]);
    let adapter = TmuxAdapter::with_runner(runner.clone());
    let metadata =
        TmuxActivationMetadata::new("host-a", "run-a", "window-a", "%7", "activation-a", "%8");

    let error = adapter
        .send_input_transaction(&metadata, "sensitive prompt")
        .unwrap_err();

    assert!(error.to_string().contains("paste-buffer"));
    assert!(!error.to_string().contains("delete-buffer"));
    assert!(runner.calls().iter().any(|call| {
        call.get(1)
            .is_some_and(|operation| operation == "delete-buffer")
    }));
}

#[test]
fn system_tmux_adapter_uses_explicit_real_tmux_binary_and_ledgers_input() {
    let _guard = lock_tmux_bin_env();
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
    let buffer_name = transaction.transaction_id().replace(':', "-");
    assert!(calls.contains(&format!("set-buffer -b {buffer_name} -- inspect the repo")));
    assert!(calls.contains(&format!(
        "paste-buffer -p -r -d -b {buffer_name} -t host-a:%7.%8"
    )));
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("display-message "))
            .count(),
        3
    );
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
