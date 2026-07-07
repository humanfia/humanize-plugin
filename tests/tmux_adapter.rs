use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use humanize_plugin::adapters::tmux::{
    CommandOutput, CommandRunner, TmuxAdapter, TmuxError, TmuxSession,
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
                "%1",
                "-v",
            ],
            vec!["tmux", "send-keys", "-t", "%2", "-l", "cargo test --quiet"],
            vec!["tmux", "capture-pane", "-p", "-t", "%2"],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
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
