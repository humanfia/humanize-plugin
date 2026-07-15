use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

use humanize_plugin::adapters::tmux::{CommandOutput, CommandRunner, TmuxAdapter, TmuxError};
use humanize_plugin::real_test::{
    DataPoint, REAL_TEST_SESSION_ID, RealTestAllocator, RealTestError, RealTestTopology, ToolKind,
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

fn data_point(tool_kind: ToolKind) -> DataPoint {
    DataPoint::new(
        "audit",
        "zlib",
        tool_kind,
        PathBuf::from("/work/projects/zlib"),
    )
}

#[test]
fn tool_kind_exposes_stable_slug_and_display_values() {
    assert_eq!(ToolKind::Codex.slug(), "codex");
    assert_eq!(ToolKind::Codex.display(), "codex");
    assert_eq!(ToolKind::Claude.slug(), "claude");
    assert_eq!(ToolKind::Claude.display(), "claude");
}

#[test]
fn real_test_topology_derives_stable_identity_fields() {
    let point = data_point(ToolKind::Codex);
    let topology = RealTestTopology::new(REAL_TEST_SESSION_ID, &point).unwrap();

    assert_eq!(topology.session_id(), REAL_TEST_SESSION_ID);
    assert_eq!(topology.window_name(), "audit");
    assert_eq!(topology.pane_label(), "zlib-codex");
    assert_eq!(
        topology.identity(),
        "humanize-plugin-real-test:audit.zlib-codex"
    );
}

#[test]
fn real_test_topology_rejects_dev_and_non_fixed_sessions() {
    let point = data_point(ToolKind::Claude);

    assert_eq!(
        RealTestTopology::new("dev", &point).unwrap_err(),
        RealTestError::InvalidSession {
            session_id: "dev".to_string()
        }
    );
    assert_eq!(
        RealTestTopology::new("host-a", &point).unwrap_err(),
        RealTestError::InvalidSession {
            session_id: "host-a".to_string()
        }
    );
}

#[test]
fn real_test_allocator_rejects_dev_and_non_fixed_sessions_before_runner_calls() {
    let runner = RecordingRunner::default();
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let point = data_point(ToolKind::Codex);

    assert_eq!(
        allocator.allocate_in_session("dev", &point).unwrap_err(),
        RealTestError::InvalidSession {
            session_id: "dev".to_string()
        }
    );
    assert_eq!(
        allocator.allocate_in_session("host-a", &point).unwrap_err(),
        RealTestError::InvalidSession {
            session_id: "host-a".to_string()
        }
    );
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
}

#[test]
fn real_test_allocator_creates_session_window_and_pane_in_order() {
    let runner = RecordingRunner::with_outputs(vec![CommandOutput::success("%7\t%8\n")]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let point = data_point(ToolKind::Claude);

    let lease = allocator.allocate(&point).unwrap();

    assert_eq!(lease.session_id(), REAL_TEST_SESSION_ID);
    assert_eq!(lease.window_id(), "%7");
    assert_eq!(lease.window_name(), "audit");
    assert_eq!(lease.pane_id(), "%8");
    assert_eq!(lease.flow_slug(), "audit");
    assert_eq!(lease.project_slug(), "zlib");
    assert_eq!(lease.tool_kind(), ToolKind::Claude);
    assert_eq!(lease.workdir(), PathBuf::from("/work/projects/zlib"));
    assert_eq!(lease.pane_label(), "zlib-claude");
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
            REAL_TEST_SESSION_ID,
            "-n",
            "audit",
        ]])
    );
}

#[test]
fn real_test_allocator_returns_existing_lease_for_repeated_data_point_without_runner_call() {
    let runner = RecordingRunner::with_outputs(vec![CommandOutput::success("%7\t%8\n")]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let point = data_point(ToolKind::Claude);

    let first_lease = allocator.allocate(&point).unwrap();
    let calls_after_first_allocation = runner.calls();
    let second_lease = allocator.allocate(&point).unwrap();

    assert_eq!(second_lease, first_lease);
    assert_eq!(runner.calls(), calls_after_first_allocation);
}

#[test]
fn real_test_allocator_fails_when_fresh_session_creation_fails_before_window_or_pane() {
    let runner = RecordingRunner::with_outputs(vec![CommandOutput::failure("duplicate session")]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let point = data_point(ToolKind::Claude);

    let err = allocator.allocate(&point).unwrap_err();

    assert_eq!(
        err,
        RealTestError::Tmux(TmuxError::command_failed(
            &vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                REAL_TEST_SESSION_ID,
                "-n",
                "audit",
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
            1,
            "duplicate session",
        ))
    );
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
            REAL_TEST_SESSION_ID,
            "-n",
            "audit",
        ]])
    );
}

#[test]
fn real_test_allocator_reuses_window_for_the_same_flow() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success("%9\n"),
    ]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let first = data_point(ToolKind::Codex);
    let second = DataPoint::new(
        "audit",
        "sqlite",
        ToolKind::Claude,
        PathBuf::from("/work/projects/sqlite"),
    );

    let first_lease = allocator.allocate(&first).unwrap();
    let second_lease = allocator.allocate(&second).unwrap();

    assert_eq!(first_lease.window_id(), "%7");
    assert_eq!(second_lease.window_id(), "%7");
    assert_eq!(second_lease.pane_id(), "%9");
    assert_eq!(
        runner.calls(),
        argv(vec![
            vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                REAL_TEST_SESSION_ID,
                "-n",
                "audit",
            ],
            vec![
                "tmux",
                "split-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                "humanize-plugin-real-test:%7",
                "-v",
            ],
        ])
    );
}

#[test]
fn real_test_allocator_creates_window_with_initial_pane_for_a_new_flow() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success("%9\t%10\n"),
    ]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let first = data_point(ToolKind::Codex);
    let second = DataPoint::new(
        "lint",
        "sqlite",
        ToolKind::Claude,
        PathBuf::from("/work/projects/sqlite"),
    );

    let first_lease = allocator.allocate(&first).unwrap();
    let second_lease = allocator.allocate(&second).unwrap();

    assert_eq!(first_lease.window_id(), "%7");
    assert_eq!(first_lease.pane_id(), "%8");
    assert_eq!(second_lease.window_id(), "%9");
    assert_eq!(second_lease.pane_id(), "%10");
    assert_eq!(
        runner.calls(),
        argv(vec![
            vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                REAL_TEST_SESSION_ID,
                "-n",
                "audit",
            ],
            vec![
                "tmux",
                "new-window",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-t",
                REAL_TEST_SESSION_ID,
                "-n",
                "lint",
            ],
        ])
    );
}

#[test]
fn real_test_allocator_release_pane_for_only_pane_clears_window_and_session_ownership() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\t%10\n"),
    ]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let point = data_point(ToolKind::Codex);
    let lease = allocator.allocate(&point).unwrap();

    allocator.release_pane(&lease).unwrap();
    let calls_after_pane_release = runner.calls();

    assert_eq!(
        allocator.release_window(&lease).unwrap_err(),
        RealTestError::UnownedWindow {
            window_id: "%7".to_string()
        }
    );
    assert_eq!(
        allocator.release_session().unwrap_err(),
        RealTestError::UnownedSession
    );
    assert_eq!(runner.calls(), calls_after_pane_release);

    let new_lease = allocator.allocate(&point).unwrap();

    assert_eq!(new_lease.window_id(), "%9");
    assert_eq!(new_lease.pane_id(), "%10");

    assert_eq!(
        runner.calls(),
        argv(vec![
            vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                REAL_TEST_SESSION_ID,
                "-n",
                "audit",
            ],
            vec!["tmux", "kill-pane", "-t", "humanize-plugin-real-test:%7.%8",],
            vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                REAL_TEST_SESSION_ID,
                "-n",
                "audit",
            ],
        ])
    );
}

#[test]
fn real_test_allocator_recreates_session_and_window_after_session_release() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\t%10\n"),
    ]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let point = data_point(ToolKind::Codex);

    let first_lease = allocator.allocate(&point).unwrap();
    allocator.release_session().unwrap();
    let second_lease = allocator.allocate(&point).unwrap();

    assert_eq!(first_lease.window_id(), "%7");
    assert_eq!(first_lease.pane_id(), "%8");
    assert_eq!(second_lease.window_id(), "%9");
    assert_eq!(second_lease.pane_id(), "%10");
    assert_eq!(
        runner.calls(),
        argv(vec![
            vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                REAL_TEST_SESSION_ID,
                "-n",
                "audit",
            ],
            vec!["tmux", "kill-session", "-t", REAL_TEST_SESSION_ID],
            vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                REAL_TEST_SESSION_ID,
                "-n",
                "audit",
            ],
        ])
    );
}

#[test]
fn real_test_allocator_release_window_invalidates_owned_pane_leases() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
    ]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let first = data_point(ToolKind::Codex);
    let second = DataPoint::new(
        "audit",
        "sqlite",
        ToolKind::Claude,
        PathBuf::from("/work/projects/sqlite"),
    );
    let first_lease = allocator.allocate(&first).unwrap();
    let second_lease = allocator.allocate(&second).unwrap();

    allocator.release_window(&first_lease).unwrap();
    let calls_after_window_release = runner.calls();

    assert_eq!(
        allocator.release_pane(&second_lease).unwrap_err(),
        RealTestError::UnownedPane {
            pane_id: "%9".to_string()
        }
    );
    assert_eq!(
        allocator.release_session().unwrap_err(),
        RealTestError::UnownedSession
    );
    assert_eq!(runner.calls(), calls_after_window_release);
}

#[test]
fn real_test_allocator_rejects_old_lease_after_session_reallocation_reuses_tmux_ids() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%7\t%8\n"),
    ]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let point = data_point(ToolKind::Codex);

    let old_lease = allocator.allocate(&point).unwrap();
    allocator.release_session().unwrap();
    let new_lease = allocator.allocate(&point).unwrap();
    let calls_after_reallocation = runner.calls();

    assert_eq!(old_lease.window_id(), new_lease.window_id());
    assert_eq!(old_lease.pane_id(), new_lease.pane_id());
    assert_eq!(
        allocator.release_pane(&old_lease).unwrap_err(),
        RealTestError::UnownedPane {
            pane_id: "%8".to_string()
        }
    );
    assert_eq!(runner.calls(), calls_after_reallocation);
}

#[test]
fn real_test_cleanup_reports_mock_runner_failure() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::failure("pane missing"),
    ]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let lease = allocator.allocate(&data_point(ToolKind::Codex)).unwrap();

    let err = allocator.release_pane(&lease).unwrap_err();

    assert_eq!(
        err,
        RealTestError::Tmux(TmuxError::command_failed(
            &vec!["tmux", "kill-pane", "-t", "humanize-plugin-real-test:%7.%8"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            1,
            "pane missing",
        ))
    );
    assert_eq!(
        runner.calls(),
        argv(vec![
            vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                REAL_TEST_SESSION_ID,
                "-n",
                "audit",
            ],
            vec!["tmux", "kill-pane", "-t", "humanize-plugin-real-test:%7.%8",],
        ])
    );
}

#[test]
fn real_test_allocator_rejects_session_release_before_allocation() {
    let runner = RecordingRunner::default();
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));

    let err = allocator.release_session().unwrap_err();

    assert_eq!(err, RealTestError::UnownedSession);
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
}

#[test]
fn real_test_allocator_rejects_session_release_after_failed_fresh_session_creation() {
    let runner = RecordingRunner::with_outputs(vec![CommandOutput::failure("duplicate session")]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let point = data_point(ToolKind::Codex);

    let allocate_err = allocator.allocate(&point).unwrap_err();
    let release_err = allocator.release_session().unwrap_err();

    assert!(matches!(allocate_err, RealTestError::Tmux(_)));
    assert_eq!(release_err, RealTestError::UnownedSession);
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
            REAL_TEST_SESSION_ID,
            "-n",
            "audit",
        ]])
    );
}

#[test]
fn real_test_allocator_rejects_repeated_pane_release_before_runner_calls() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
    ]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let lease = allocator.allocate(&data_point(ToolKind::Codex)).unwrap();

    allocator.release_pane(&lease).unwrap();
    let calls_after_release = runner.calls();
    let err = allocator.release_pane(&lease).unwrap_err();

    assert_eq!(
        err,
        RealTestError::UnownedPane {
            pane_id: "%8".to_string()
        }
    );
    assert_eq!(runner.calls(), calls_after_release);
}

#[test]
fn real_test_allocator_rejects_pane_release_from_another_allocator_before_runner_calls() {
    let owner_runner = RecordingRunner::with_outputs(vec![CommandOutput::success("%7\t%8\n")]);
    let other_runner = RecordingRunner::default();
    let mut owner = RealTestAllocator::new(TmuxAdapter::with_runner(owner_runner));
    let mut other = RealTestAllocator::new(TmuxAdapter::with_runner(other_runner.clone()));
    let lease = owner.allocate(&data_point(ToolKind::Codex)).unwrap();

    let err = other.release_pane(&lease).unwrap_err();

    assert_eq!(
        err,
        RealTestError::UnownedPane {
            pane_id: "%8".to_string()
        }
    );
    assert_eq!(other_runner.calls(), Vec::<Vec<String>>::new());
}

#[test]
fn real_test_allocator_rejects_window_release_from_another_allocator_before_runner_calls() {
    let owner_runner = RecordingRunner::with_outputs(vec![CommandOutput::success("%7\t%8\n")]);
    let other_runner = RecordingRunner::default();
    let mut owner = RealTestAllocator::new(TmuxAdapter::with_runner(owner_runner));
    let mut other = RealTestAllocator::new(TmuxAdapter::with_runner(other_runner.clone()));
    let lease = owner.allocate(&data_point(ToolKind::Codex)).unwrap();

    let err = other.release_window(&lease).unwrap_err();

    assert_eq!(
        err,
        RealTestError::UnownedWindow {
            window_id: "%7".to_string()
        }
    );
    assert_eq!(other_runner.calls(), Vec::<Vec<String>>::new());
}

#[test]
fn real_test_allocator_keeps_ownership_when_session_release_fails() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::failure("session busy"),
        CommandOutput::success(""),
    ]);
    let mut allocator = RealTestAllocator::new(TmuxAdapter::with_runner(runner.clone()));
    let lease = allocator.allocate(&data_point(ToolKind::Codex)).unwrap();

    let err = allocator.release_session().unwrap_err();
    allocator.release_pane(&lease).unwrap();

    assert!(matches!(err, RealTestError::Tmux(_)));
    assert_eq!(
        runner.calls(),
        argv(vec![
            vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                REAL_TEST_SESSION_ID,
                "-n",
                "audit",
            ],
            vec!["tmux", "kill-session", "-t", REAL_TEST_SESSION_ID],
            vec!["tmux", "kill-pane", "-t", "humanize-plugin-real-test:%7.%8",],
        ])
    );
}
