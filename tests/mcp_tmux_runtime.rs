mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::adapters::tmux::{CommandOutput, CommandRunner};
use humanize_plugin::mcp::McpServer;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::json;

use support::mcp::{RecordingRunner, call_tool, lock_flow, structured};

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
            "#{window_id}\t#{pane_id}",
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
            "#{window_id}\t#{pane_id}",
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
                {
                    "id": "finish",
                    "action": {
                        "driver": "agent"
                    }
                }
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
            "#{window_id}\t#{pane_id}",
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
fn run_flow_locked_agent_node_warns_without_agent_command() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
    ]);
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

    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(structured(&started)["actuation"]["sent"], json!([]));
    assert_eq!(
        structured(&started)["actuation_warnings"],
        json!([
            {
                "activation_id": "root",
                "node_id": "root",
                "driver": "agent",
                "message": "tmux.agent_command is required before autonomous agent actuation"
            }
        ])
    );
    let calls = runner.calls();
    assert_eq!(calls.len(), 3);
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
            "#{window_id}\t#{pane_id}",
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
                "agent_command": "humanize-test-agent"
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
    assert_eq!(machine_inputs[1]["role"], "node_prompt");
    assert_eq!(machine_inputs[1]["normalized_text"], expected_prompt);
    assert_eq!(machine_inputs[1]["status"], "submitted");
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
            "#{window_id}\t#{pane_id}",
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
            "#{session_name}\t#{window_id}\t#{window_name}\t#{pane_id}",
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
            "#{session_name}\t#{window_id}\t#{window_name}\t#{pane_id}",
        ]])
        .remove(0)
    );
    assert_eq!(
        calls[7],
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
        calls[8],
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
fn run_flow_locked_unsupported_driver_reports_actuation_warning_in_status() {
    let mut server = isolated_default_server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_script_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-script-warning",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false
        }),
    );

    let expected_warning = json!({
        "activation_id": "root",
        "node_id": "root",
        "driver": "script",
        "message": "action driver is not supported for autonomous tmux actuation"
    });
    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(
        structured(&started)["actuation_warnings"],
        json!([expected_warning])
    );

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-script-warning"
        }),
    );

    assert_eq!(
        structured(&status)["context"]["actuation_warnings"],
        json!([expected_warning])
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
            "#{window_id}\t#{pane_id}",
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
            "#{session_name}\t#{window_id}\t#{window_name}\t#{pane_id}",
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
