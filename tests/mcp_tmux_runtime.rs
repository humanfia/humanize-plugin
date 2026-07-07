mod support;

use humanize_plugin::adapters::tmux::CommandOutput;
use humanize_plugin::mcp::McpServer;
use serde_json::json;

use support::mcp::{RecordingRunner, call_tool, lock_valid_flow, structured};

#[test]
fn start_run_reports_tmux_disabled_without_static_creation_claim() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-tmux",
            "nodes": ["root"]
        }),
    );

    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(structured(&started)["tmux"]["enabled"], false);
    assert_eq!(structured(&started)["tmux"]["created"], false);
    assert!(structured(&started).get("tmux_mapping").is_none());
}

#[test]
fn start_run_creates_explicit_tmux_window_with_activation_panes() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%9\t%10\n"),
        CommandOutput::success("%11\n"),
    ]);
    let mut server = McpServer::with_tmux_runner(runner.clone());

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-tmux-created",
            "nodes": ["root", "reviewer"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "audit-run"
            }
        }),
    );

    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(
        structured(&started)["activation_ids"],
        json!(["root", "reviewer"])
    );
    assert_eq!(structured(&started)["tmux"]["window_id"], "%9");
    assert_eq!(
        structured(&started)["tmux"]["panes"],
        json!([
            {
                "activation_id": "root",
                "pane_id": "%10",
                "session_id": "host-a",
                "window_id": "%9",
                "window_name": "audit-run"
            },
            {
                "activation_id": "reviewer",
                "pane_id": "%11",
                "session_id": "host-a",
                "window_id": "%9",
                "window_name": "audit-run"
            }
        ])
    );
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
                "#{window_id}\t#{pane_id}",
                "-s",
                "host-a",
                "-n",
                "audit-run",
            ],
            vec![
                "tmux",
                "split-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                "host-a:%9",
                "-v",
            ],
        ])
    );
}

#[test]
fn start_run_rejects_reserved_dev_tmux_session_before_runner_calls() {
    let runner = RecordingRunner::default();
    let mut server = McpServer::with_tmux_runner(runner.clone());

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-dev",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "dev",
                "window": "audit-run"
            }
        }),
    );

    assert_eq!(started["error"]["code"], -32602);
    assert!(
        started["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("tmux session named dev is reserved")
    );
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());

    let context = call_tool(
        &mut server,
        2,
        "get_context",
        json!({
            "run_id": "run-dev"
        }),
    );
    assert_eq!(context["error"]["code"], -32602);
}

#[test]
fn start_run_allows_dedicated_real_test_tmux_session() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
    ]);
    let mut server = McpServer::with_tmux_runner(runner.clone());

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-real-test",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "humanize-plugin-real-test",
                "window": "audit-run"
            }
        }),
    );

    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(
        runner.calls(),
        argv(vec![
            vec!["tmux", "has-session", "-t", "humanize-plugin-real-test"],
            vec![
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}\t#{pane_id}",
                "-s",
                "humanize-plugin-real-test",
                "-n",
                "audit-run",
            ],
        ])
    );
}

#[test]
fn start_run_returns_error_when_tmux_creation_fails_without_starting_runtime() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::failure("new-session failed"),
    ]);
    let mut server = McpServer::with_tmux_runner(runner.clone());

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-tmux-failed",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "audit-run"
            }
        }),
    );

    assert_eq!(started["error"]["code"], -32602);
    let context = call_tool(
        &mut server,
        2,
        "get_context",
        json!({
            "run_id": "run-tmux-failed"
        }),
    );
    assert_eq!(context["error"]["code"], -32602);
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
                "#{window_id}\t#{pane_id}",
                "-s",
                "host-a",
                "-n",
                "audit-run",
            ],
        ])
    );
}

#[test]
fn resume_run_allocates_tmux_panes_for_route_created_activations() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success("%9\n"),
    ]);
    let mut server = McpServer::with_tmux_runner(runner.clone());
    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-resume-routes",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["activation_ids"], json!(["root"]));

    call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-resume-routes",
            "activation_id": "root",
            "artifact_key": "ready",
            "payload": "true"
        }),
    );
    let paused = call_tool(
        &mut server,
        4,
        "pause_run",
        json!({
            "run_id": "run-resume-routes"
        }),
    );
    assert_eq!(structured(&paused)["run_status"], "paused");

    let resumed = call_tool(
        &mut server,
        5,
        "resume_run",
        json!({
            "run_id": "run-resume-routes"
        }),
    );

    assert_eq!(structured(&resumed)["run_status"], "running");
    assert_eq!(
        structured(&resumed)["tmux_allocations"],
        json!([
            {
                "activation_id": "finish",
                "pane_id": "%9",
                "session_id": "host-a",
                "window_id": "%7",
                "window_name": "flow-a"
            }
        ])
    );
}

#[test]
fn observe_stop_releases_satisfied_tmux_activation_pane() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
    ]);
    let mut server = McpServer::with_tmux_runner(runner.clone());

    let started = call_tool(
        &mut server,
        1,
        "run_flow",
        json!({
            "run_id": "run-tmux-release",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(
        structured(&started)["tmux"]["panes"],
        json!([
            {
                "activation_id": "root",
                "pane_id": "%8",
                "session_id": "host-a",
                "window_id": "%7",
                "window_name": "flow-a"
            }
        ])
    );

    let observed = call_tool(
        &mut server,
        2,
        "observe_stop",
        json!({
            "run_id": "run-tmux-release",
            "activation_id": "root",
            "reason": "pane exited"
        }),
    );
    assert_eq!(structured(&observed)["ok"], true);
    assert_eq!(structured(&observed)["tmux_cleanup"]["action"], "kill_pane");
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
                "#{window_id}\t#{pane_id}",
                "-s",
                "host-a",
                "-n",
                "flow-a",
            ],
            vec!["tmux", "kill-pane", "-t", "host-a:%7.%8"],
        ])
    );
}

fn argv(commands: Vec<Vec<&str>>) -> Vec<Vec<String>> {
    commands
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect()
}
