mod support;

use humanize_plugin::adapters::tmux::CommandOutput;
use humanize_plugin::mcp::McpServer;
use serde_json::json;

use support::mcp::{RecordingRunner, call_tool, structured};

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
fn start_run_creates_explicit_tmux_window_without_panes() {
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
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
    assert_eq!(structured(&started)["tmux"]["enabled"], true);
    assert_eq!(structured(&started)["tmux"]["created"], true);
    assert_eq!(structured(&started)["tmux"]["session_id"], "host-a");
    assert_eq!(structured(&started)["tmux"]["window_id"], "%9");
    assert_eq!(structured(&started)["tmux"]["window_name"], "audit-run");
    assert_eq!(structured(&started)["tmux"]["run_id"], "run-tmux-created");
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
                "audit-run",
            ],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
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
        CommandOutput::success(""),
        CommandOutput::success("%7\n"),
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
        vec![
            vec!["tmux", "has-session", "-t", "humanize-plugin-real-test"],
            vec![
                "tmux",
                "new-session",
                "-d",
                "-s",
                "humanize-plugin-real-test",
            ],
            vec![
                "tmux",
                "new-window",
                "-P",
                "-F",
                "#{window_id}",
                "-t",
                "humanize-plugin-real-test",
                "-n",
                "audit-run",
            ],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
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
    assert!(
        started["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("tmux")
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
    assert!(
        context["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("run-tmux-failed")
    );
    assert_eq!(
        runner.calls(),
        vec![
            vec!["tmux", "has-session", "-t", "host-a"],
            vec!["tmux", "new-session", "-d", "-s", "host-a"],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
    );
}
#[test]
fn get_context_keeps_existing_runtime_context_fields() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-context",
            "nodes": [
                {
                    "id": "root",
                    "required_artifacts": ["brief"],
                    "required_effects": ["shell"]
                }
            ]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    call_tool(
        &mut server,
        2,
        "deliver_artifact",
        json!({
            "run_id": "run-context",
            "activation_id": "root",
            "artifact_key": "brief",
            "payload": "ready"
        }),
    );
    call_tool(
        &mut server,
        3,
        "record_effect",
        json!({
            "run_id": "run-context",
            "activation_id": "root",
            "effect_key": "shell",
            "payload": "ok"
        }),
    );
    call_tool(
        &mut server,
        4,
        "patch_board",
        json!({
            "run_id": "run-context",
            "activation_id": "root",
            "patch": {
                "summary": "ready"
            }
        }),
    );
    call_tool(
        &mut server,
        5,
        "send_message",
        json!({
            "run_id": "run-context",
            "message": {
                "role": "user",
                "content": "hello"
            }
        }),
    );

    let context = call_tool(
        &mut server,
        6,
        "get_context",
        json!({
            "run_id": "run-context"
        }),
    );
    let context = structured(&context)["context"]
        .as_object()
        .expect("context should be an object");
    let keys = context.keys().cloned().collect::<Vec<_>>();

    assert_eq!(
        keys,
        vec![
            "activation_ids",
            "activations",
            "artifacts",
            "board",
            "board_version",
            "effects",
            "flow_lock_applications",
            "flow_lock_mode",
            "latest_artifact_by_slot_index",
            "latest_flow_lock_application",
            "message_count",
            "run_id",
        ]
    );
    assert_eq!(context["run_id"], "run-context");
    assert_eq!(context["activation_ids"], json!(["root"]));
    assert_eq!(context["board_version"], 1);
    assert_eq!(context["message_count"], 1);
    assert_eq!(context["effects"]["root:shell"], "ok");
}
#[test]
fn mcp_rejects_cross_run_deliver_and_validate_stop() {
    let mut server = McpServer::new();

    let run_a = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-a",
            "nodes": [
                {
                    "id": "only-a",
                    "required_artifacts": ["brief"]
                }
            ]
        }),
    );
    assert_eq!(structured(&run_a)["activation_ids"], json!(["only-a"]));

    let run_b = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-b",
            "nodes": ["only-b"]
        }),
    );
    assert_eq!(structured(&run_b)["activation_ids"], json!(["only-b"]));

    let delivered = call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-b",
            "activation_id": "only-a",
            "artifact_key": "brief",
            "payload": "wrong run"
        }),
    );
    assert_eq!(delivered["error"]["code"], -32602);
    assert!(
        delivered["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("run-b")
    );

    let validated = call_tool(
        &mut server,
        4,
        "validate_stop",
        json!({
            "run_id": "run-b",
            "activation_id": "only-a"
        }),
    );
    assert_eq!(validated["result"]["isError"], true);
    assert_eq!(structured(&validated)["missing"], json!(["activation"]));
    assert!(
        structured(&validated)["error"]
            .as_str()
            .expect("error should include a message")
            .contains("run-b")
    );
}
#[test]
fn validate_stop_uses_activation_contract_before_and_after_artifact_delivery() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-stop",
            "nodes": [
                {
                    "id": "root",
                    "required_artifacts": ["brief"]
                }
            ]
        }),
    );
    assert_eq!(structured(&started)["activation_ids"], json!(["root"]));

    let blocked = call_tool(
        &mut server,
        2,
        "validate_stop",
        json!({
            "run_id": "run-stop",
            "activation_id": "root"
        }),
    );
    assert_eq!(blocked["result"]["isError"], true);
    assert_eq!(structured(&blocked)["valid"], false);
    assert_eq!(structured(&blocked)["missing"], json!(["artifact:brief"]));

    let delivered = call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-stop",
            "activation_id": "root",
            "artifact_key": "brief",
            "payload": "ready"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);

    let allowed = call_tool(
        &mut server,
        4,
        "validate_stop",
        json!({
            "run_id": "run-stop",
            "activation_id": "root"
        }),
    );
    assert_eq!(structured(&allowed)["valid"], true);
    assert_eq!(structured(&allowed)["missing"], json!([]));
}
