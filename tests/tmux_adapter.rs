use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use humanize_plugin::adapters::hooks::{
    DriverDecision, HookAction, StopHookInput, build_stop_hook_payload,
};
use humanize_plugin::adapters::lifecycle::{
    AgentLifecycleAdapter, LifecycleCleanupAction, LifecycleStatus,
};
use humanize_plugin::adapters::tmux::{
    CommandOutput, CommandRunner, TmuxActivationMetadata, TmuxActivationRequest, TmuxAdapter,
    TmuxError, TmuxInputTransactionConfig, TmuxPane, TmuxPaneIdentity, TmuxPaneMetadataMismatch,
    TmuxSession, TmuxWindow,
};
use humanize_plugin::input_ledger::{
    MachineInputLedger, MachineInputStatus, machine_input_payload_hash,
};

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

fn argv(rows: Vec<Vec<&str>>) -> Vec<Vec<String>> {
    rows.into_iter()
        .map(|row| row.into_iter().map(String::from).collect())
        .collect()
}

fn test_temp_dir(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(name);
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    path
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
        machine_input_payload_hash("inspect\nthe repo")
    );
    assert_eq!(records[0].submit_key_count, 2);
    assert_eq!(records[0].transaction_id, transaction.transaction_id());
    assert_eq!(records[0].status, MachineInputStatus::Started);
    assert_eq!(records[1].transaction_id, transaction.transaction_id());
    assert_eq!(records[1].status, MachineInputStatus::Submitted);
    assert!(records[0].transaction_id.starts_with("machine-input:"));
    assert_eq!(
        runner.calls(),
        argv(vec![
            vec![
                "tmux",
                "display-message",
                "-p",
                "-t",
                "host-a:%7.%8",
                "#{session_name}\t#{window_id}\t#{window_name}\t#{pane_id}",
            ],
            vec![
                "tmux",
                "send-keys",
                "-t",
                "host-a:%7.%8",
                "-l",
                "inspect\r\nthe repo",
            ],
            vec!["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"],
            vec!["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"],
        ])
    );
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
            "#{session_name}\t#{window_id}\t#{window_name}\t#{pane_id}",
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
            "#{session_name}\t#{window_id}\t#{window_name}\t#{pane_id}",
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
            "#{session_name}\t#{window_id}\t#{window_name}\t#{pane_id}",
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
            "#{window_id}\t#{pane_id}",
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
            "#{window_id}\t#{pane_id}",
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
        TmuxError::CommandFailed {
            argv: vec!["tmux", "kill-pane", "-t", "host-a:%1.%2"]
                .into_iter()
                .map(String::from)
                .collect(),
            status: 1,
            stderr: "pane missing".to_string()
        }
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
    assert_eq!(records[0].status, MachineInputStatus::Started);
    assert_eq!(records[1].status, MachineInputStatus::Submitted);
    assert_eq!(records[2].run_id, "run-a");
    assert_eq!(records[2].activation_id, "activation-a");
    assert_eq!(records[2].normalized_text, "inspect the repo");
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
                "#{session_name}\t#{window_id}\t#{window_name}\t#{pane_id}",
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
                "#{session_name}\t#{window_id}\t#{window_name}\t#{pane_id}",
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
                "#{window_name}\t#{window_id}",
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

#[test]
fn stop_hook_helper_maps_driver_decision_to_neutral_allow_and_block_payloads() {
    let input = StopHookInput::new("session-a", "activation-a");

    let allow = build_stop_hook_payload(&input, DriverDecision::Allow);
    let block = build_stop_hook_payload(
        &input,
        DriverDecision::Block {
            reason: "contract is not satisfied".to_string(),
        },
    );

    assert_eq!(allow.action(), HookAction::Allow);
    assert_eq!(allow.reason(), None);
    assert_eq!(allow.session_id(), "session-a");
    assert_eq!(allow.activation_id(), "activation-a");
    assert_eq!(block.action(), HookAction::Block);
    assert_eq!(block.reason(), Some("contract is not satisfied"));
    assert_eq!(block.session_id(), "session-a");
    assert_eq!(block.activation_id(), "activation-a");
}
