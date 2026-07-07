use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use humanize_plugin::adapters::tmux::{
    CommandOutput, CommandRunner, TmuxAdapter, TmuxError, TmuxPane, TmuxSession, TmuxWindow,
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
