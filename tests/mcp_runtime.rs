mod support;

use humanize_plugin::adapters::tmux::CommandOutput;
use humanize_plugin::mcp::McpServer;
use serde_json::{Value, json};

use support::mcp::{
    RecordingRunner, assert_tool_error, call_tool, lock_flow, lock_valid_flow, readme_resource,
    structured,
};

fn flow_for_each_preview() -> Value {
    json!({
        "nodes": [
            { "id": "root" },
            { "id": "process" }
        ],
        "resources": [readme_resource()],
        "routes": [
            {
                "predicate": "exists(artifact.ready)",
                "for_each": "artifact.items",
                "activate": "process"
            }
        ]
    })
}

fn flow_with_board_routes() -> Value {
    json!({
        "nodes": [
            { "id": "root" },
            { "id": "exists_target" },
            { "id": "bare_target" }
        ],
        "resources": [readme_resource()],
        "routes": [
            {
                "predicate": "exists(board.ready)",
                "activate": "exists_target"
            },
            {
                "predicate": "board.ready",
                "activate": "bare_target"
            }
        ]
    })
}

fn flow_with_event_named_fact_routes() -> Value {
    json!({
        "nodes": [
            { "id": "root" },
            { "id": "artifact_target" },
            { "id": "board_target" }
        ],
        "resources": [readme_resource()],
        "routes": [
            {
                "predicate": "artifact.event.status",
                "activate": "artifact_target"
            },
            {
                "predicate": "board.event.ready",
                "activate": "board_target"
            }
        ]
    })
}

fn flow_with_unsupported_routes() -> Value {
    json!({
        "nodes": [
            { "id": "root" },
            { "id": "event_target" },
            { "id": "exists_event_target" },
            { "id": "equality_target" }
        ],
        "resources": [readme_resource()],
        "routes": [
            {
                "predicate": "event.completed",
                "activate": "event_target"
            },
            {
                "predicate": "exists(event.review_requested)",
                "activate": "exists_event_target"
            },
            {
                "predicate": "artifact.schema == 'event.v1'",
                "activate": "equality_target"
            }
        ]
    })
}

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

#[test]
fn preview_flow_routes_uses_explicit_lock_without_runtime_mutation() {
    let mut server = McpServer::new();

    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);
    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-preview-explicit",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let delivered = call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-preview-explicit",
            "activation_id": "root",
            "artifact_key": "ready",
            "payload": "ready"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);

    let preview = call_tool(
        &mut server,
        4,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-explicit",
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );

    assert_eq!(structured(&preview)["ok"], true);
    assert_eq!(structured(&preview)["run_id"], "run-preview-explicit");
    assert_eq!(structured(&preview)["flow_lock_id"], lock_id);
    assert_eq!(structured(&preview)["lock_id"], lock_id);
    assert_eq!(structured(&preview)["content_hash"], content_hash);
    assert_eq!(structured(&preview)["source"], "explicit");
    assert_eq!(
        structured(&preview)["routes"],
        json!([
            {
                "route_index": 0,
                "activate": "finish",
                "predicate": "exists(artifact.ready)",
                "matched": true,
                "reason": null,
                "for_each": null,
                "planned_activations": [
                    {
                        "activation_id": "finish",
                        "stable_key": null
                    }
                ]
            }
        ])
    );

    let context = call_tool(
        &mut server,
        5,
        "get_context",
        json!({
            "run_id": "run-preview-explicit"
        }),
    );
    assert_eq!(
        structured(&context)["context"]["activation_ids"],
        json!(["root"])
    );
}

#[test]
fn preview_flow_routes_uses_latest_applied_lock_by_default() {
    let mut server = McpServer::new();

    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);
    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-preview-latest",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let applied = call_tool(
        &mut server,
        3,
        "apply_flow_lock",
        json!({
            "run_id": "run-preview-latest",
            "mode": "future_activations",
            "lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&applied)["ok"], true);
    let delivered = call_tool(
        &mut server,
        4,
        "deliver_artifact",
        json!({
            "run_id": "run-preview-latest",
            "activation_id": "root",
            "artifact_key": "ready",
            "payload": "ready"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);

    let preview = call_tool(
        &mut server,
        5,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-latest"
        }),
    );

    assert_eq!(structured(&preview)["ok"], true);
    assert_eq!(structured(&preview)["source"], "latest_applied");
    assert_eq!(structured(&preview)["flow_lock_id"], lock_id);
    assert_eq!(structured(&preview)["content_hash"], content_hash);
    assert_eq!(structured(&preview)["routes"][0]["matched"], true);
}

#[test]
fn preview_flow_routes_without_latest_lock_returns_tool_error() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-preview-no-lock",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let preview = call_tool(
        &mut server,
        2,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-no-lock"
        }),
    );

    assert_tool_error(&preview);
    assert_eq!(structured(&preview)["run_id"], "run-preview-no-lock");
    assert_eq!(structured(&preview)["error"], "flow_lock_id is required");
}

#[test]
fn preview_flow_routes_rejects_content_hash_mismatch() {
    let mut server = McpServer::new();

    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);
    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-preview-hash",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let preview = call_tool(
        &mut server,
        3,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-hash",
            "flowLockId": lock_id,
            "contentHash": "fnv1a64:0000000000000000"
        }),
    );

    assert_tool_error(&preview);
    assert_eq!(structured(&preview)["flow_lock_id"], lock_id);
    assert_eq!(structured(&preview)["lock_id"], lock_id);
    assert_eq!(
        structured(&preview)["content_hash"],
        "fnv1a64:0000000000000000"
    );
    assert_eq!(structured(&preview)["expected_content_hash"], content_hash);
    assert_eq!(
        structured(&preview)["error"],
        "flow lock content hash mismatch"
    );
}

#[test]
fn preview_flow_routes_fans_out_artifact_lines_without_runtime_mutation() {
    let mut server = McpServer::new();

    let (lock_id, _) = lock_flow(&mut server, 1, flow_for_each_preview());
    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-preview-for-each",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let ready = call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-preview-for-each",
            "activation_id": "root",
            "artifact_key": "ready",
            "payload": "ready"
        }),
    );
    assert_eq!(structured(&ready)["ok"], true);
    let items = call_tool(
        &mut server,
        4,
        "deliver_artifact",
        json!({
            "run_id": "run-preview-for-each",
            "activation_id": "root",
            "artifact_key": "items",
            "payload": "alpha\nbeta"
        }),
    );
    assert_eq!(structured(&items)["ok"], true);

    let preview = call_tool(
        &mut server,
        5,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-for-each",
            "lock_id": lock_id
        }),
    );

    assert_eq!(structured(&preview)["ok"], true);
    assert_eq!(
        structured(&preview)["routes"][0]["planned_activations"],
        json!([
            {
                "activation_id": "process:items/0",
                "stable_key": "items/0",
                "index": 0,
                "item": "alpha"
            },
            {
                "activation_id": "process:items/1",
                "stable_key": "items/1",
                "index": 1,
                "item": "beta"
            }
        ])
    );

    let context = call_tool(
        &mut server,
        6,
        "get_context",
        json!({
            "run_id": "run-preview-for-each"
        }),
    );
    assert_eq!(
        structured(&context)["context"]["activation_ids"],
        json!(["root"])
    );
}

#[test]
fn preview_flow_routes_reports_duplicate_fanout_activation_without_partial_plan() {
    let mut server = McpServer::new();

    let (lock_id, _) = lock_flow(&mut server, 1, flow_for_each_preview());
    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-preview-duplicate",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-preview-duplicate",
            "activation_id": "root",
            "artifact_key": "ready",
            "payload": "ready"
        }),
    );
    call_tool(
        &mut server,
        4,
        "deliver_artifact",
        json!({
            "run_id": "run-preview-duplicate",
            "activation_id": "root",
            "artifact_key": "items",
            "payload": "alpha\nbeta"
        }),
    );
    let fanout = call_tool(
        &mut server,
        5,
        "fanout_from_artifact",
        json!({
            "run_id": "run-preview-duplicate",
            "node_id": "process",
            "artifact_key": "items",
            "for_each": "items"
        }),
    );
    assert_eq!(structured(&fanout)["ok"], true);

    let preview = call_tool(
        &mut server,
        6,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-duplicate",
            "lock_id": lock_id
        }),
    );

    assert_eq!(structured(&preview)["ok"], true);
    assert_eq!(structured(&preview)["routes"][0]["matched"], false);
    assert_eq!(
        structured(&preview)["routes"][0]["reason"],
        "duplicate activation: process:items/0"
    );
    assert_eq!(
        structured(&preview)["routes"][0]["planned_activations"],
        json!([])
    );
}

#[test]
fn preview_flow_routes_distinguishes_board_presence_from_bare_truthiness() {
    let mut server = McpServer::new();

    let (lock_id, _) = lock_flow(&mut server, 1, flow_with_board_routes());
    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-preview-board",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let patched = call_tool(
        &mut server,
        3,
        "patch_board",
        json!({
            "run_id": "run-preview-board",
            "activation_id": "root",
            "patch": {
                "ready": false
            }
        }),
    );
    assert_eq!(structured(&patched)["ok"], true);

    let preview = call_tool(
        &mut server,
        4,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-board",
            "lock_id": lock_id
        }),
    );

    assert_eq!(structured(&preview)["ok"], true);
    assert_eq!(structured(&preview)["routes"][0]["matched"], true);
    assert_eq!(
        structured(&preview)["routes"][0]["planned_activations"],
        json!([
            {
                "activation_id": "exists_target",
                "stable_key": null
            }
        ])
    );
    assert_eq!(structured(&preview)["routes"][1]["matched"], false);
    assert_eq!(
        structured(&preview)["routes"][1]["reason"],
        "predicate_unmatched"
    );
}

#[test]
fn preview_flow_routes_matches_artifact_and_board_paths_containing_event() {
    let mut server = McpServer::new();

    let (lock_id, _) = lock_flow(&mut server, 1, flow_with_event_named_fact_routes());
    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-preview-event-named-facts",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let delivered = call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-preview-event-named-facts",
            "activation_id": "root",
            "artifact_key": "event.status",
            "payload": "ready"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);
    let patched = call_tool(
        &mut server,
        4,
        "patch_board",
        json!({
            "run_id": "run-preview-event-named-facts",
            "activation_id": "root",
            "patch": {
                "event.ready": true
            }
        }),
    );
    assert_eq!(structured(&patched)["ok"], true);

    let preview = call_tool(
        &mut server,
        5,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-event-named-facts",
            "lock_id": lock_id
        }),
    );

    assert_eq!(structured(&preview)["ok"], true);
    assert_eq!(structured(&preview)["routes"][0]["matched"], true);
    assert_eq!(structured(&preview)["routes"][0]["reason"], Value::Null);
    assert_eq!(
        structured(&preview)["routes"][0]["planned_activations"],
        json!([
            {
                "activation_id": "artifact_target",
                "stable_key": null
            }
        ])
    );
    assert_eq!(structured(&preview)["routes"][1]["matched"], true);
    assert_eq!(structured(&preview)["routes"][1]["reason"], Value::Null);
    assert_eq!(
        structured(&preview)["routes"][1]["planned_activations"],
        json!([
            {
                "activation_id": "board_target",
                "stable_key": null
            }
        ])
    );
}

#[test]
fn preview_flow_routes_reports_event_and_unsupported_predicates_per_route() {
    let mut server = McpServer::new();

    let (lock_id, _) = lock_flow(&mut server, 1, flow_with_unsupported_routes());
    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-preview-unsupported",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let delivered = call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-preview-unsupported",
            "activation_id": "root",
            "artifact_key": "schema",
            "payload": "x"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);

    let preview = call_tool(
        &mut server,
        4,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-unsupported",
            "lock_id": lock_id
        }),
    );

    assert_eq!(structured(&preview)["ok"], true);
    assert_eq!(structured(&preview)["routes"][0]["matched"], false);
    assert_eq!(
        structured(&preview)["routes"][0]["reason"],
        "event fact source unavailable"
    );
    assert_eq!(structured(&preview)["routes"][1]["matched"], false);
    assert_eq!(
        structured(&preview)["routes"][1]["reason"],
        "event fact source unavailable"
    );
    assert_eq!(structured(&preview)["routes"][2]["matched"], false);
    assert_eq!(
        structured(&preview)["routes"][2]["reason"],
        "unsupported_predicate"
    );
}

#[test]
fn fanout_from_artifact_returns_activation_metadata_and_context() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-fanout",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let delivered = call_tool(
        &mut server,
        2,
        "deliver_artifact",
        json!({
            "run_id": "run-fanout",
            "activation_id": "root",
            "artifact_key": "items",
            "payload": "alpha\nbeta"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);

    let fanout = call_tool(
        &mut server,
        3,
        "fanout_from_artifact",
        json!({
            "run_id": "run-fanout",
            "node_id": "process",
            "artifact_key": "items",
            "forEach": "items",
            "required_artifacts": ["done"],
            "required_effects": ["shell"]
        }),
    );

    assert_eq!(structured(&fanout)["ok"], true);
    assert_eq!(structured(&fanout)["run_id"], "run-fanout");
    assert_eq!(structured(&fanout)["node_id"], "process");
    assert_eq!(structured(&fanout)["artifact_key"], "items");
    assert_eq!(structured(&fanout)["activation_count"], 2);
    assert_eq!(
        structured(&fanout)["activation_ids"],
        json!(["process:items/0", "process:items/1"])
    );
    assert_eq!(
        structured(&fanout)["activations"],
        json!([
            {
                "activation_id": "process:items/0",
                "stable_key": "items/0"
            },
            {
                "activation_id": "process:items/1",
                "stable_key": "items/1"
            }
        ])
    );

    let context = call_tool(
        &mut server,
        4,
        "get_context",
        json!({
            "run_id": "run-fanout"
        }),
    );
    let activation = &structured(&context)["context"]["activations"]["process:items/0"];
    assert_eq!(activation["stable_key"], "items/0");
    assert_eq!(activation["context"]["for_each"], "items");
    assert_eq!(activation["context"]["index"], "0");
    assert_eq!(activation["context"]["item"], "alpha");
    assert_eq!(activation["required_artifacts"], json!(["done"]));
    assert_eq!(activation["required_effects"], json!(["shell"]));
}

#[test]
fn fanout_from_artifact_missing_artifact_returns_error_without_activation() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-missing-fanout",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let fanout = call_tool(
        &mut server,
        2,
        "fanout_from_artifact",
        json!({
            "run_id": "run-missing-fanout",
            "node_id": "process",
            "artifact_key": "items"
        }),
    );

    assert_eq!(fanout["error"]["code"], -32602);
    assert!(
        fanout["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("artifact not found: items")
    );

    let context = call_tool(
        &mut server,
        3,
        "get_context",
        json!({
            "run_id": "run-missing-fanout"
        }),
    );
    assert_eq!(
        structured(&context)["context"]["activation_ids"],
        json!(["root"])
    );
}

#[test]
fn fanout_from_artifact_for_each_mismatch_returns_error_without_activation() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-mismatch-fanout",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let delivered = call_tool(
        &mut server,
        2,
        "deliver_artifact",
        json!({
            "run_id": "run-mismatch-fanout",
            "activation_id": "root",
            "artifact_key": "items",
            "payload": "alpha"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);

    let fanout = call_tool(
        &mut server,
        3,
        "fanout_from_artifact",
        json!({
            "run_id": "run-mismatch-fanout",
            "node_id": "process",
            "artifact_key": "items",
            "for_each": "other"
        }),
    );

    assert_eq!(fanout["error"]["code"], -32602);
    assert!(
        fanout["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("for_each mismatch: expected other, actual items")
    );

    let context = call_tool(
        &mut server,
        4,
        "get_context",
        json!({
            "run_id": "run-mismatch-fanout"
        }),
    );
    assert_eq!(
        structured(&context)["context"]["activation_ids"],
        json!(["root"])
    );
}
