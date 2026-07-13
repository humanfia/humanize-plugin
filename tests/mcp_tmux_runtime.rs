mod support;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::adapters::tmux::{CommandOutput, CommandRunner};
use humanize_plugin::mcp::{McpServer, TmuxExecutionDefaults};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::json;

use support::mcp::{RecordingRunner, SideEffectRunner, call_tool, lock_flow, structured};

static NEXT_ASSET_ROOT: AtomicU64 = AtomicU64::new(1);

fn isolated_server<R: CommandRunner>(runner: R) -> McpServer<R> {
    let index = NEXT_ASSET_ROOT.fetch_add(1, Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("mcp-tmux-runtime-assets-{index}"));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    McpServer::with_tmux_runner_and_run_asset_store(
        runner,
        RunAssetStore::new(RunAssetSink::Root(root)),
    )
}

fn isolated_default_server() -> McpServer<RecordingRunner> {
    isolated_server(RecordingRunner::default())
}

fn isolated_server_with_defaults<R: CommandRunner>(
    runner: R,
    defaults: TmuxExecutionDefaults,
) -> McpServer<R> {
    let index = NEXT_ASSET_ROOT.fetch_add(1, Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("mcp-tmux-runtime-assets-with-defaults-{index}"));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    McpServer::with_tmux_runner_run_asset_store_and_execution_defaults(
        runner,
        RunAssetStore::new(RunAssetSink::Root(root)),
        defaults,
    )
}

#[test]
fn start_run_reports_tmux_disabled_without_static_creation_claim() {
    let mut server = isolated_default_server();

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
        CommandOutput::success(""),
        CommandOutput::success("%11\n"),
        CommandOutput::success(""),
    ]);
    let mut server = isolated_server(runner.clone());

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
    let calls = runner.calls();
    assert_eq!(calls.len(), 5);
    assert_eq!(
        calls[0],
        argv(vec![vec!["tmux", "has-session", "-t", "host-a"]]).remove(0)
    );
    assert_eq!(
        calls[1],
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
            "audit-run",
        ]])
        .remove(0)
    );
    assert_pipe_command(&calls[2], "host-a:%9.%10");
    assert_eq!(
        calls[3],
        argv(vec![vec![
            "tmux",
            "split-window",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "host-a:%9",
            "-v",
        ]])
        .remove(0)
    );
    assert_pipe_command(&calls[4], "host-a:%9.%11");
}

#[test]
fn start_run_rejects_reserved_dev_tmux_session_before_runner_calls() {
    let runner = RecordingRunner::default();
    let mut server = isolated_server(runner.clone());

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
        CommandOutput::success(""),
    ]);
    let mut server = isolated_server(runner.clone());

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
    let calls = runner.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[0],
        argv(vec![vec![
            "tmux",
            "has-session",
            "-t",
            "humanize-plugin-real-test"
        ]])
        .remove(0)
    );
    assert_eq!(
        calls[1],
        argv(vec![vec![
            "tmux",
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{window_id}|#{pane_id}",
            "-s",
            "humanize-plugin-real-test",
            "-n",
            "audit-run",
        ]])
        .remove(0)
    );
    assert_pipe_command(&calls[2], "humanize-plugin-real-test:%7.%8");
}

#[test]
fn start_run_returns_error_when_tmux_creation_fails_without_starting_runtime() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::failure("new-session failed"),
    ]);
    let mut server = isolated_server(runner.clone());

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

    assert_eq!(started["result"]["isError"], true);
    assert_eq!(structured(&started)["ok"], false);
    assert_eq!(
        structured(&started)["asset_preservation"]["status"],
        "failed"
    );
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
                "#{window_id}|#{pane_id}",
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
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
    ]);
    let mut server = isolated_server(runner.clone());
    let (lock_id, content_hash) = lock_flow(
        &mut server,
        1,
        json!({
            "nodes": [
                { "id": "root" },
                { "id": "finish" }
            ],
            "resources": [support::mcp::readme_resource()],
            "routes": [
                {
                    "predicate": "exists(artifact.ready)",
                    "activate": "finish"
                }
            ]
        }),
    );

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
        CommandOutput::success("final transcript\n"),
        CommandOutput::success(""),
    ]);
    let mut server = isolated_server(runner.clone());

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
    let calls = runner.calls();
    assert_eq!(calls.len(), 5);
    assert_eq!(
        calls[0],
        argv(vec![vec!["tmux", "has-session", "-t", "host-a"]]).remove(0)
    );
    assert_eq!(
        calls[1],
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
            "flow-a",
        ]])
        .remove(0)
    );
    assert_pipe_command(&calls[2], "host-a:%7.%8");
    assert_eq!(
        calls[3],
        argv(vec![vec![
            "tmux",
            "capture-pane",
            "-p",
            "-t",
            "host-a:%7.%8"
        ]])
        .remove(0)
    );
    assert_eq!(
        calls[4],
        argv(vec![vec!["tmux", "kill-pane", "-t", "host-a:%7.%8"]]).remove(0)
    );
}

#[test]
fn run_flow_locked_agent_node_requires_tmux_context_before_start() {
    let runner = RecordingRunner::default();
    let mut server = isolated_server(runner.clone());
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-agent-no-command",
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

    assert_eq!(structured(&started)["ok"], false);
    assert_eq!(
        structured(&started)["error"],
        "autonomous tmux execution context required"
    );
    assert_eq!(
        structured(&started)["missing"],
        json!(["tmux.agent_command"])
    );
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-agent-no-command"
        }),
    );
    assert_eq!(status["error"]["code"], -32602);
}

#[test]
fn run_flow_locked_agent_node_without_tmux_object_fails_before_start() {
    let runner = RecordingRunner::default();
    let mut server = isolated_server(runner.clone());
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-agent-no-tmux-object",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false
        }),
    );

    assert_eq!(structured(&started)["ok"], false);
    assert_eq!(
        structured(&started)["error"],
        "autonomous tmux execution context required"
    );
    assert_eq!(
        structured(&started)["missing"],
        json!(["tmux.session", "tmux.agent_command"])
    );
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-agent-no-tmux-object"
        }),
    );
    assert_eq!(status["error"]["code"], -32602);
}

#[test]
fn run_flow_locked_agent_node_respects_explicit_tmux_disabled_over_defaults() {
    let runner = RecordingRunner::default();
    let defaults = TmuxExecutionDefaults {
        session: Some("host-a".into()),
        window: None,
        agent_command: Some("humanize-test-agent".into()),
    };
    let mut server = isolated_server_with_defaults(runner.clone(), defaults);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-agent-disabled-tmux",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": false
            }
        }),
    );

    assert_eq!(structured(&started)["ok"], false);
    assert_eq!(
        structured(&started)["error"],
        "autonomous tmux execution context required"
    );
    assert_eq!(structured(&started)["missing"], json!(["tmux.enabled"]));
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-agent-disabled-tmux"
        }),
    );
    assert_eq!(status["error"]["code"], -32602);
}

#[test]
fn run_flow_locked_agent_node_merges_explicit_session_with_default_command() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("explicit-host\t%7\trun-agent-partial-tmux\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success("explicit-host\t%7\trun-agent-partial-tmux\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
    ]);
    let defaults = TmuxExecutionDefaults {
        session: Some("default-host".into()),
        window: None,
        agent_command: Some("humanize-test-agent".into()),
    };
    let mut server = isolated_server_with_defaults(runner.clone(), defaults);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-agent-partial-tmux",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "explicit-host"
            }
        }),
    );

    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(structured(&started)["tmux"]["session_id"], "explicit-host");
    assert_eq!(
        structured(&started)["tmux"]["window_name"],
        "run-agent-partial-tmux"
    );
    let sent = &structured(&started)["actuation"]["sent"][0];
    assert_eq!(sent["agent_command"], "humanize-test-agent");
    assert_eq!(sent["session_id"], "explicit-host");
    let calls = runner.calls();
    assert_eq!(
        calls[0],
        argv(vec![vec!["tmux", "has-session", "-t", "explicit-host"]]).remove(0)
    );
}

#[test]
fn run_flow_locked_agent_node_uses_configured_default_tmux_context() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\trun-agent-defaults\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\trun-agent-defaults\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
    ]);
    let defaults = TmuxExecutionDefaults {
        session: Some("host-a".into()),
        window: None,
        agent_command: Some("humanize-test-agent".into()),
    };
    let mut server = isolated_server_with_defaults(runner.clone(), defaults);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-agent-defaults",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false
        }),
    );

    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(structured(&started)["tmux"]["enabled"], true);
    assert_eq!(
        structured(&started)["tmux"]["window_name"],
        "run-agent-defaults"
    );
    assert_eq!(structured(&started)["actuation_warnings"], json!([]));
    let sent = &structured(&started)["actuation"]["sent"][0];
    assert_eq!(sent["driver"], "agent");
    assert_eq!(sent["agent_command"], "humanize-test-agent");
    let calls = runner.calls();
    assert_eq!(calls.len(), 9);
    assert_eq!(
        calls[0],
        argv(vec![vec!["tmux", "has-session", "-t", "host-a"]]).remove(0)
    );
    assert_eq!(
        calls[1],
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
            "run-agent-defaults",
        ]])
        .remove(0)
    );
}

#[test]
fn run_flow_suggested_flow_uses_bare_artifact_key_for_stop_and_routes() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\trun-suggested-route\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\trun-suggested-route\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\trun-suggested-route\t%9\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\trun-suggested-route\t%9\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
    ]);
    let defaults = TmuxExecutionDefaults {
        session: Some("host-a".into()),
        window: None,
        agent_command: Some("humanize-test-agent".into()),
    };
    let mut server = isolated_server_with_defaults(runner, defaults);
    let suggested = call_tool(
        &mut server,
        1,
        "flow_suggest",
        json!({
            "goal": "Draft a concise migration brief.",
            "nodes": ["Collect facts", "Review output"],
            "artifact": "Brief"
        }),
    );
    let mut flow = structured(&suggested)["flow"].clone();
    flow["routes"] = json!([
        {
            "predicate": "exists(artifact.brief)",
            "activate": "review_output"
        }
    ]);
    assert!(
        flow["resources"][3]["source"]
            .as_str()
            .expect("prompt source should be a string")
            .contains("artifact_key \"brief\"")
    );
    assert!(
        !flow["resources"][3]["source"]
            .as_str()
            .expect("prompt source should be a string")
            .contains("artifact.brief")
    );
    let (lock_id, content_hash) = lock_flow(&mut server, 2, flow);

    let started = call_tool(
        &mut server,
        3,
        "run_flow",
        json!({
            "run_id": "run-suggested-route",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false
        }),
    );
    assert_eq!(
        structured(&started)["activation_ids"],
        json!(["collect_facts"])
    );

    let delivered = call_tool(
        &mut server,
        4,
        "deliver_artifact",
        json!({
            "run_id": "run-suggested-route",
            "activation_id": "collect_facts",
            "artifact_key": "brief",
            "payload": "ready"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);
    assert_eq!(structured(&delivered)["artifact_key"], "brief");

    let observed = call_tool(
        &mut server,
        5,
        "observe_stop",
        json!({
            "run_id": "run-suggested-route",
            "activation_id": "collect_facts",
            "reason": "brief delivered"
        }),
    );
    assert_eq!(structured(&observed)["ok"], true);
    assert_eq!(
        structured(&observed)["stop_decisions"][0]["decision"],
        "allow"
    );
    assert_eq!(
        structured(&observed)["tmux_allocations"][0]["activation_id"],
        "review_output"
    );
}

#[test]
fn run_flow_locked_agent_node_launches_agent_then_sends_initial_prompt() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\tflow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\tflow-a\t%8\n"),
        CommandOutput::success("Use /skills to list available skills\n"),
        CommandOutput::success("host-a\t%7\tflow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
    ]);
    let mut server = isolated_server(runner.clone());
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-agent-prompt",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a",
                "agent_command": "humanize-test-agent",
                "agent_ready_pattern": "Use /skills to list available skills",
                "agent_ready_timeout_ms": 60000,
                "prompt_submit_key_count": 1
            }
        }),
    );

    let expected_prompt = "Inspect the repository.\n\nResources:\nreadme.main (readme): Use Humanize to audit this library without editing files.";
    assert_eq!(structured(&started)["ok"], true);
    let sent = &structured(&started)["actuation"]["sent"][0];
    assert_eq!(sent["activation_id"], "root");
    assert_eq!(sent["node_id"], "root");
    assert_eq!(sent["driver"], "agent");
    assert_eq!(sent["agent_command"], "humanize-test-agent");
    assert_eq!(
        sent["agent_ready_pattern"],
        "Use /skills to list available skills"
    );
    assert_eq!(sent["prompt_submit_key_count"], 1);
    assert_eq!(sent["pane_id"], "%8");
    assert_eq!(sent["session_id"], "host-a");
    assert_eq!(sent["window_id"], "%7");
    assert_eq!(sent["window_name"], "flow-a");
    assert_eq!(structured(&started)["actuation_warnings"], json!([]));
    assert!(
        sent["agent_launch_transaction_id"]
            .as_str()
            .unwrap()
            .starts_with("machine-input:")
    );
    assert!(
        sent["prompt_transaction_id"]
            .as_str()
            .unwrap()
            .starts_with("machine-input:")
    );

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-agent-prompt"
        }),
    );
    let machine_inputs = structured(&status)["context"]["machine_inputs"]
        .as_array()
        .expect("machine inputs should be exposed in run status");
    assert_eq!(machine_inputs.len(), 2);
    assert_eq!(machine_inputs[0]["role"], "agent_launch");
    assert_eq!(machine_inputs[0]["normalized_text"], "humanize-test-agent");
    assert_eq!(machine_inputs[0]["status"], "submitted");
    assert_eq!(machine_inputs[0]["submit_key_count"], 1);
    assert_eq!(machine_inputs[1]["role"], "node_prompt");
    assert_eq!(machine_inputs[1]["normalized_text"], expected_prompt);
    assert_eq!(machine_inputs[1]["status"], "submitted");
    assert_eq!(machine_inputs[1]["submit_key_count"], 1);
    let calls = runner.calls();
    assert_eq!(calls.len(), 11);
    assert_eq!(
        calls[0],
        argv(vec![vec!["tmux", "has-session", "-t", "host-a"]]).remove(0)
    );
    assert_eq!(
        calls[1],
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
            "flow-a",
        ]])
        .remove(0)
    );
    assert_pipe_command(&calls[2], "host-a:%7.%8");
    assert_eq!(
        calls[3],
        argv(vec![vec![
            "tmux",
            "display-message",
            "-p",
            "-t",
            "host-a:%7.%8",
            "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
        ]])
        .remove(0)
    );
    assert_eq!(
        calls[4],
        argv(vec![vec![
            "tmux",
            "send-keys",
            "-t",
            "host-a:%7.%8",
            "-l",
            "humanize-test-agent",
        ]])
        .remove(0)
    );
    assert_eq!(
        calls[5],
        argv(vec![vec![
            "tmux",
            "send-keys",
            "-t",
            "host-a:%7.%8",
            "Enter"
        ]])
        .remove(0)
    );
    assert_eq!(
        calls[6],
        argv(vec![vec![
            "tmux",
            "display-message",
            "-p",
            "-t",
            "host-a:%7.%8",
            "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
        ]])
        .remove(0)
    );
    assert_eq!(
        calls[7],
        argv(vec![vec![
            "tmux",
            "capture-pane",
            "-p",
            "-t",
            "host-a:%7.%8"
        ]])
        .remove(0)
    );
    assert_eq!(
        calls[8],
        argv(vec![vec![
            "tmux",
            "display-message",
            "-p",
            "-t",
            "host-a:%7.%8",
            "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
        ]])
        .remove(0)
    );
    assert_eq!(
        calls[9],
        argv(vec![vec![
            "tmux",
            "send-keys",
            "-t",
            "host-a:%7.%8",
            "-l",
            expected_prompt,
        ]])
        .remove(0)
    );
    assert_eq!(
        calls[10],
        argv(vec![vec![
            "tmux",
            "send-keys",
            "-t",
            "host-a:%7.%8",
            "Enter"
        ]])
        .remove(0)
    );
}

#[test]
fn run_flow_locked_review_node_launches_agent_with_review_prompt() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\trun-review-defaults\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\trun-review-defaults\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
    ]);
    let defaults = TmuxExecutionDefaults {
        session: Some("host-a".into()),
        window: None,
        agent_command: Some("humanize-test-agent".into()),
    };
    let mut server = isolated_server_with_defaults(runner.clone(), defaults);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_review_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-review-defaults",
            "flow_lock_id": lock_id.clone(),
            "content_hash": content_hash,
            "review_required": false
        }),
    );

    let expected_prompt = "Review the collected facts.\n\nResources:\nreadme.main (readme): Use Humanize to audit this library without editing files.";
    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(structured(&started)["actuation_warnings"], json!([]));
    let sent = &structured(&started)["actuation"]["sent"][0];
    assert_eq!(sent["activation_id"], "review");
    assert_eq!(sent["node_id"], "review");
    assert_eq!(sent["driver"], "review");
    assert_eq!(sent["agent_command"], "humanize-test-agent");

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-review-defaults"
        }),
    );
    let machine_inputs = structured(&status)["context"]["machine_inputs"]
        .as_array()
        .expect("machine inputs should be exposed in run status");
    assert_eq!(machine_inputs[0]["role"], "agent_launch");
    assert_eq!(machine_inputs[1]["role"], "node_prompt");
    assert_eq!(machine_inputs[1]["normalized_text"], expected_prompt);
    assert_eq!(runner.calls().len(), 9);

    let exported = call_tool(
        &mut server,
        4,
        "flow_export",
        json!({
            "flow_lock_id": lock_id,
            "format": "json"
        }),
    );
    let document = structured(&exported)["document"]
        .as_str()
        .expect("flow_export should include a document");
    let exported_json = serde_json::from_str::<serde_json::Value>(document)
        .expect("exported review flow should be JSON");
    let content = exported_json["content"]
        .as_str()
        .expect("exported review flow should include normalized content");
    assert!(content.contains("\"driver\":\"review\""));
    assert!(content.contains("\"prompt_ref\":\"prompt.review\""));
    assert!(content.contains("\"verdict_artifact\":\"artifact.review_verdict\""));
}

#[test]
fn run_flow_locked_human_node_waits_without_tmux_actuation() {
    let runner = RecordingRunner::default();
    let mut server = isolated_server(runner.clone());
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_human_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-human-wait",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false
        }),
    );

    let waiting = json!([
        {
            "activation_id": "human_review",
            "node_id": "human_review",
            "driver": "human",
            "status": "waiting_human",
            "message": "human action is waiting for external input"
        }
    ]);
    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(structured(&started)["tmux"]["enabled"], false);
    assert_eq!(structured(&started)["actuation"]["sent"], json!([]));
    assert_eq!(structured(&started)["actuation"]["waiting_human"], waiting);
    assert_eq!(structured(&started)["actuation_warnings"], json!([]));
    assert_eq!(runner.calls(), Vec::<Vec<String>>::new());

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-human-wait"
        }),
    );
    assert_eq!(structured(&status)["context"]["waiting_human"], waiting);
}

#[cfg(unix)]
#[test]
fn run_flow_propagates_durable_machine_input_store_error() {
    use std::os::unix::fs::PermissionsExt;

    let index = NEXT_ASSET_ROOT.fetch_add(1, Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("mcp-tmux-runtime-machine-input-store-{index}"));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    let run_id = "run-machine-input-store-failure";
    let store = RunAssetStore::new(RunAssetSink::Root(root));
    let ledger_path = store.run_root(run_id).unwrap().join("machine-inputs.jsonl");
    let runner = SideEffectRunner::with_outputs(
        vec![
            CommandOutput::failure("missing session"),
            CommandOutput::success("%7\t%8\n"),
            CommandOutput::success(""),
            CommandOutput::success("host-a\t%7\tflow-a\t%8\n"),
            CommandOutput::success(""),
            CommandOutput::success(""),
        ],
        move |argv| {
            if argv.get(1).map(String::as_str) == Some("pipe-pane") {
                fs::write(&ledger_path, "").unwrap();
                let mut permissions = fs::metadata(&ledger_path).unwrap().permissions();
                permissions.set_mode(0o400);
                fs::set_permissions(&ledger_path, permissions).unwrap();
            }
        },
    );
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": run_id,
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a",
                "agent_command": "humanize-test-agent"
            }
        }),
    );

    assert!(
        started["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("run asset preservation")
    );
    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": run_id
        }),
    );
    assert_eq!(structured(&status)["context"]["run_status"], "failed");
    let run_assets = &structured(&status)["context"]["run_assets"];
    assert_eq!(
        run_assets["preservation_errors"][0]["stage"],
        "machine_input"
    );
    assert!(
        run_assets["preservation_errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("machine input ledger")
    );
    assert_eq!(
        run_assets["records"]["files"]["preservation"]["record_count"],
        1
    );
}

#[test]
fn run_status_exposes_stop_decision_detail_after_observe_stop() {
    let mut server = isolated_default_server();

    call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-stop-detail",
            "nodes": [
                {
                    "id": "root",
                    "required_artifacts": ["summary"]
                }
            ]
        }),
    );
    call_tool(
        &mut server,
        2,
        "observe_stop",
        json!({
            "run_id": "run-stop-detail",
            "activation_id": "root",
            "reason": "agent stopped"
        }),
    );

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-stop-detail"
        }),
    );

    let decisions = structured(&status)["context"]["stop_decisions"]
        .as_array()
        .expect("stop decisions should be exposed in run status");
    assert_eq!(decisions.len(), 1);
    assert!(
        decisions[0]["decision_id"]
            .as_str()
            .unwrap()
            .starts_with("event:")
    );
    assert_eq!(decisions[0]["activation_id"], "root");
    assert_eq!(decisions[0]["decision"], "deny");
    assert_eq!(decisions[0]["attempt"], 1);
    assert_eq!(decisions[0]["reason"], "missing stop requirements");
    assert_eq!(decisions[0]["missing"], json!(["artifact:summary"]));
}

#[test]
fn flow_lock_rejects_unsupported_script_driver_before_run() {
    let mut server = isolated_default_server();
    let locked = call_tool(
        &mut server,
        1,
        "flow_lock",
        json!({
            "flow": locked_script_flow()
        }),
    );

    assert_eq!(locked["result"]["isError"], true);
    assert_eq!(
        structured(&locked)["diagnostics"][0]["code"],
        "FLOW_UNSUPPORTED_SCRIPT_ACTION_DRIVER"
    );
}

#[test]
fn run_status_and_view_snapshot_expose_locked_run_archive_fields() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\tflow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\tflow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
    ]);
    let mut server = isolated_server(runner);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-archive",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a",
                "agent_command": "humanize-test-agent"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-archive"
        }),
    );
    let context = &structured(&status)["context"];
    assert_eq!(context["flow_lock_id"], lock_id);
    assert_eq!(context["content_hash"], content_hash);
    assert_eq!(context["flow_review_status"], "not_required");
    assert!(
        context["flow_export_document"]
            .as_str()
            .unwrap()
            .contains("root")
    );
    assert_eq!(context["event_count"], 8);
    assert_eq!(context["event_timeline"].as_array().unwrap().len(), 8);
    assert_eq!(
        context["pane_mappings"],
        json!([
            {
                "activation_id": "root",
                "run_id": "run-archive",
                "pane": "host-a:%7.%8",
                "session_id": "host-a",
                "window_id": "%7",
                "window_name": "flow-a",
                "pane_id": "%8",
                "status": "running"
            }
        ])
    );

    let snapshot = call_tool(
        &mut server,
        4,
        "view_snapshot",
        json!({
            "run_id": "run-archive"
        }),
    );
    let run = &structured(&snapshot)["snapshot"]["runs"][0];
    assert_eq!(run["flow_lock_id"], lock_id);
    assert_eq!(run["content_hash"], content_hash);
    assert_eq!(run["event_count"], 8);
    assert_eq!(run["pane_mappings"], context["pane_mappings"]);
}

#[test]
fn run_flow_does_not_send_agent_prompt_before_tmux_metadata_validation() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\tother-window\t%8\n"),
    ]);
    let mut server = isolated_server(runner.clone());
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-metadata-mismatch",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a",
                "agent_command": "humanize-test-agent"
            }
        }),
    );

    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(
        structured(&started)["actuation_warnings"][0]["message"],
        "tmux actuation failed before agent launch"
    );
    let calls = runner.calls();
    assert_eq!(calls.len(), 4);
    assert_eq!(
        calls[0],
        argv(vec![vec!["tmux", "has-session", "-t", "host-a"]]).remove(0)
    );
    assert_eq!(
        calls[1],
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
            "flow-a",
        ]])
        .remove(0)
    );
    assert_eq!(
        &calls[2][..5],
        ["tmux", "pipe-pane", "-o", "-t", "host-a:%7.%8"]
    );
    assert_pipe_command(&calls[2], "host-a:%7.%8");
    assert_eq!(
        calls[3],
        argv(vec![vec![
            "tmux",
            "display-message",
            "-p",
            "-t",
            "host-a:%7.%8",
            "#{session_name}|#{window_id}|#{window_name}|#{pane_id}",
        ]])
        .remove(0)
    );
}

fn locked_agent_flow() -> serde_json::Value {
    json!({
        "nodes": [
            {
                "id": "root",
                "action": {
                    "driver": "agent",
                    "prompt_ref": "prompt.start",
                    "resource_refs": ["readme.main"]
                }
            }
        ],
        "resources": [
            {
                "id": "readme.main",
                "kind": "readme",
                "source": "inline:Use Humanize to audit this library without editing files."
            },
            {
                "id": "prompt.start",
                "kind": "prompt",
                "source": "inline:Inspect the repository."
            }
        ]
    })
}

fn locked_script_flow() -> serde_json::Value {
    json!({
        "nodes": [
            {
                "id": "root",
                "action": {
                    "driver": "script",
                    "resource_refs": ["script.collect"]
                }
            }
        ],
        "resources": [
            {
                "id": "readme.main",
                "kind": "readme",
                "source": "inline:Use Humanize to audit this library without editing files."
            },
            {
                "id": "script.collect",
                "kind": "script",
                "source": "scripts/collect.sh"
            }
        ]
    })
}

fn locked_review_flow() -> serde_json::Value {
    json!({
        "nodes": [
            {
                "id": "review",
                "action": {
                    "driver": "review",
                    "prompt_ref": "prompt.review",
                    "resource_refs": ["readme.main"],
                    "writes": ["artifact.review_verdict"],
                    "verdict_artifact": "artifact.review_verdict"
                }
            }
        ],
        "resources": [
            {
                "id": "readme.main",
                "kind": "readme",
                "source": "inline:Use Humanize to audit this library without editing files."
            },
            {
                "id": "prompt.review",
                "kind": "prompt",
                "source": "inline:Review the collected facts."
            }
        ]
    })
}

fn locked_human_flow() -> serde_json::Value {
    json!({
        "nodes": [
            {
                "id": "human_review",
                "action": {
                    "driver": "human",
                    "writes": ["artifact.human_decision"]
                }
            }
        ],
        "resources": [
            {
                "id": "readme.main",
                "kind": "readme",
                "source": "inline:Wait for a human decision before continuing."
            }
        ]
    })
}

fn assert_pipe_command(call: &[String], target: &str) {
    assert_eq!(&call[..5], ["tmux", "pipe-pane", "-o", "-t", target]);
    let command = &call[5];
    assert!(command.contains("--pipe-sink"));
    assert!(command.contains("--root"));
    assert!(command.contains("--relative"));
    assert!(command.contains("--dev"));
    assert!(command.contains("--ino"));
    assert!(command.contains("--ack-relative"));
    assert!(command.contains("transcript.pipe.log"));
    assert!(!command.contains("cat >>"));
}

fn argv(commands: Vec<Vec<&str>>) -> Vec<Vec<String>> {
    commands
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect()
}
