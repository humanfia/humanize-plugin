mod support;

use humanize_plugin::mcp::McpServer;
use serde_json::{Value, json};

use support::mcp::{
    assert_tool_error, call_tool, lock_flow, lock_valid_flow, readme_resource, structured,
    valid_flow,
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

fn flow_with_locked_root_contract() -> Value {
    json!({
        "nodes": [
            { "id": "locked_root", "contract_id": "contract.locked_root" },
            { "id": "locked_review", "contract_id": "contract.locked_review" }
        ],
        "contracts": [
            {
                "id": "contract.locked_root",
                "completion": "all_artifacts",
                "artifacts": [
                    {
                        "id": "brief",
                        "schema_resource_id": "schema.brief"
                    }
                ]
            },
            {
                "id": "contract.locked_review",
                "completion": "manual",
                "artifacts": []
            }
        ],
        "resources": [
            readme_resource(),
            {
                "id": "schema.brief",
                "kind": "schema",
                "source": "inline:brief"
            }
        ],
        "routes": [
            {
                "predicate": "exists(artifact.brief)",
                "activate": "locked_review"
            }
        ]
    })
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
    for key in [
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
    ] {
        assert!(
            context.contains_key(key),
            "context should retain existing field {key}"
        );
    }
    for key in [
        "event_timeline",
        "last_decision",
        "pane_mappings",
        "run_status",
        "runtime_budgets",
        "why",
    ] {
        assert!(
            context.contains_key(key),
            "context should expose runtime view field {key}"
        );
    }
    assert_eq!(context["run_id"], "run-context");
    assert_eq!(context["activation_ids"], json!(["root"]));
    assert_eq!(context["board_version"], 1);
    assert_eq!(context["message_count"], 1);
    assert_eq!(context["effects"]["root:shell"], "ok");
}

#[test]
fn get_context_includes_stop_contract_summary() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "run_flow",
        json!({
            "runId": "run-context-stop-summary",
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

    let status = call_tool(
        &mut server,
        2,
        "run_status",
        json!({
            "runId": "run-context-stop-summary"
        }),
    );
    assert_eq!(
        structured(&status)["context"]["missing_stop_contract_count"],
        2
    );
    assert_eq!(
        structured(&status)["context"]["missing_stop_contracts"]["root"],
        json!(["artifact:brief", "effect:shell"])
    );

    let context = call_tool(
        &mut server,
        3,
        "get_context",
        json!({
            "runId": "run-context-stop-summary"
        }),
    );
    assert_eq!(
        structured(&context)["context"]["missing_stop_contract_count"],
        structured(&status)["context"]["missing_stop_contract_count"]
    );
    assert_eq!(
        structured(&context)["context"]["missing_stop_contracts"],
        structured(&status)["context"]["missing_stop_contracts"]
    );
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
fn run_flow_requires_prepared_review_when_review_is_required() {
    let mut server = McpServer::new();
    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);

    let blocked = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-review-gated",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": true,
            "nodes": ["root"]
        }),
    );

    assert_tool_error(&blocked);
    assert_eq!(structured(&blocked)["ok"], false);
    assert_eq!(structured(&blocked)["error"], "flow review required");
    assert_eq!(structured(&blocked)["next_tool"], "prepare_flow_review");
    assert_eq!(
        structured(&blocked)["after_next_tool"],
        "approve_flow_review"
    );
}

#[test]
fn run_flow_starts_reviewed_run_and_exposes_status_and_cause() {
    let mut server = McpServer::new();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, flow_with_locked_root_contract());
    let prepared = call_tool(
        &mut server,
        2,
        "prepare_flow_review",
        json!({
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&prepared)["ok"], true);
    let review_id = structured(&prepared)["review_id"]
        .as_str()
        .expect("review should include id");
    let approved = call_tool(
        &mut server,
        3,
        "approve_flow_review",
        json!({
            "review_id": review_id,
            "decision": "approved"
        }),
    );
    assert_eq!(structured(&approved)["review_status"], "approved");

    let started = call_tool(
        &mut server,
        4,
        "run_flow",
        json!({
            "run_id": "run-reviewed",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": true
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(structured(&started)["run_id"], "run-reviewed");
    assert_eq!(structured(&started)["run_status"], "running");
    assert_eq!(structured(&started)["flow_lock_id"], lock_id);

    let status = call_tool(
        &mut server,
        5,
        "run_status",
        json!({
            "run_id": "run-reviewed"
        }),
    );
    assert_eq!(structured(&status)["ok"], true);
    assert_eq!(structured(&status)["run_status"], "running");
    assert_eq!(
        structured(&status)["context"]["activation_ids"],
        json!(["locked_root"])
    );
    assert_eq!(
        structured(&status)["context"]["missing_stop_contracts"]["locked_root"],
        json!(["artifact:brief"])
    );
    let activation = &structured(&status)["context"]["activations"]["locked_root"];
    assert_eq!(activation["status"], "running");
    assert_eq!(activation["flow_lock_mode"], "future_activations");
    assert_eq!(activation["flow_lock_id"], lock_id);
    assert_eq!(activation["contract_hash"], content_hash);

    let why = call_tool(
        &mut server,
        6,
        "run_why",
        json!({
            "run_id": "run-reviewed"
        }),
    );
    assert_eq!(structured(&why)["ok"], true);
    assert_eq!(structured(&why)["cause"], "missing stop requirements");
}

#[test]
fn run_flow_uses_locked_draft_nodes_and_contracts_when_lock_supplied() {
    let mut server = McpServer::new();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, flow_with_locked_root_contract());
    let prepared = call_tool(
        &mut server,
        2,
        "prepare_flow_review",
        json!({
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&prepared)["ok"], true);
    let review_id = structured(&prepared)["review_id"]
        .as_str()
        .expect("review should include id");
    let approved = call_tool(
        &mut server,
        3,
        "approve_flow_review",
        json!({
            "review_id": review_id,
            "decision": "approved"
        }),
    );
    assert_eq!(structured(&approved)["review_status"], "approved");

    let started = call_tool(
        &mut server,
        4,
        "run_flow",
        json!({
            "run_id": "run-locked-nodes",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": true,
            "nodes": [
                {
                    "id": "unrelated",
                    "required_artifacts": ["intruder"]
                }
            ]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(
        structured(&started)["activation_ids"],
        json!(["locked_root"])
    );

    let status = call_tool(
        &mut server,
        5,
        "run_status",
        json!({
            "run_id": "run-locked-nodes"
        }),
    );
    assert_eq!(
        structured(&status)["context"]["activation_ids"],
        json!(["locked_root"])
    );
    assert_eq!(
        structured(&status)["context"]["missing_stop_contracts"]["locked_root"],
        json!(["artifact:brief"])
    );
    let activation = &structured(&status)["context"]["activations"]["locked_root"];
    assert_eq!(activation["status"], "running");
    assert_eq!(activation["flow_lock_mode"], "future_activations");
    assert_eq!(activation["flow_lock_id"], lock_id);
    assert_eq!(activation["contract_hash"], content_hash);
    assert!(
        structured(&status)["context"]["missing_stop_contracts"]
            .get("unrelated")
            .is_none()
    );
}

#[test]
fn pause_resume_and_stop_run_use_runtime_control_statuses() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "run_flow",
        json!({
            "run_id": "run-control",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["run_status"], "running");

    let paused = call_tool(
        &mut server,
        2,
        "pause_run",
        json!({
            "run_id": "run-control"
        }),
    );
    assert_eq!(structured(&paused)["ok"], true);
    assert_eq!(structured(&paused)["run_status"], "paused");

    let resumed = call_tool(
        &mut server,
        3,
        "resume_run",
        json!({
            "run_id": "run-control"
        }),
    );
    assert_eq!(structured(&resumed)["ok"], true);
    assert_eq!(structured(&resumed)["run_status"], "running");

    let stopped = call_tool(
        &mut server,
        4,
        "stop_run",
        json!({
            "run_id": "run-control"
        }),
    );
    assert_eq!(structured(&stopped)["ok"], true);
    assert_eq!(structured(&stopped)["run_status"], "stopped");
}

#[test]
fn observe_stop_records_observation_and_advances_driver() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "run_flow",
        json!({
            "run_id": "run-observed-stop",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["run_status"], "running");

    let observed = call_tool(
        &mut server,
        2,
        "observe_stop",
        json!({
            "run_id": "run-observed-stop",
            "activation_id": "root",
            "reason": "pane exited"
        }),
    );

    assert_eq!(structured(&observed)["ok"], true);
    assert_eq!(structured(&observed)["run_id"], "run-observed-stop");
    assert_eq!(structured(&observed)["activation_id"], "root");
    assert_eq!(structured(&observed)["run_status"], "completed");
    assert_eq!(
        structured(&observed)["stop_decisions"],
        json!([
            {
                "activation_id": "root",
                "decision": "allow",
                "attempt": 1,
                "reason": null,
                "missing": []
            }
        ])
    );

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-observed-stop"
        }),
    );
    assert_eq!(structured(&status)["run_status"], "completed");
}

#[test]
fn mcp_locked_flow_routes_activate_after_stop_observation() {
    let mut server = McpServer::new();
    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);
    let prepared = call_tool(
        &mut server,
        2,
        "prepare_flow_review",
        json!({
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&prepared)["ok"], true);
    let review_id = structured(&prepared)["review_id"]
        .as_str()
        .expect("review should include id");
    let approved = call_tool(
        &mut server,
        3,
        "approve_flow_review",
        json!({
            "review_id": review_id,
            "decision": "approved"
        }),
    );
    assert_eq!(structured(&approved)["review_status"], "approved");

    let started = call_tool(
        &mut server,
        4,
        "run_flow",
        json!({
            "run_id": "run-mcp-routed",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": true
        }),
    );
    assert_eq!(structured(&started)["activation_ids"], json!(["root"]));

    let delivered = call_tool(
        &mut server,
        5,
        "deliver_artifact",
        json!({
            "run_id": "run-mcp-routed",
            "activation_id": "root",
            "artifact_key": "ready",
            "payload": "true"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);

    let observed = call_tool(
        &mut server,
        6,
        "observe_stop",
        json!({
            "run_id": "run-mcp-routed",
            "activation_id": "root",
            "reason": "pane exited"
        }),
    );
    assert_eq!(structured(&observed)["ok"], true);
    assert_eq!(structured(&observed)["run_status"], "running");

    let status = call_tool(
        &mut server,
        7,
        "run_status",
        json!({
            "run_id": "run-mcp-routed"
        }),
    );
    assert_eq!(
        structured(&status)["context"]["activation_ids"],
        json!(["finish", "root"])
    );
    assert_eq!(
        structured(&status)["context"]["activations"]["finish"]["status"],
        "running"
    );
}

#[test]
fn apply_flow_update_records_runtime_application() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-update",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let proposed = call_tool(
        &mut server,
        2,
        "propose_flow_update",
        json!({
            "flow": valid_flow(),
            "apply_mode": "checkpoint_restart",
            "summary": "Switch to locked update flow."
        }),
    );
    assert_eq!(structured(&proposed)["ok"], true);
    let lock_id = structured(&proposed)["flow_lock_id"]
        .as_str()
        .expect("proposal should include lock")
        .to_string();
    let content_hash = structured(&proposed)["content_hash"]
        .as_str()
        .expect("proposal should include hash")
        .to_string();
    let prepared = call_tool(
        &mut server,
        3,
        "prepare_flow_review",
        json!({
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&prepared)["ok"], true);
    let review_id = structured(&prepared)["review_id"]
        .as_str()
        .expect("review should include id");
    let approved = call_tool(
        &mut server,
        4,
        "approve_flow_review",
        json!({
            "review_id": review_id,
            "decision": "approved"
        }),
    );
    assert_eq!(structured(&approved)["review_status"], "approved");

    let applied = call_tool(
        &mut server,
        5,
        "apply_flow_update",
        json!({
            "run_id": "run-update",
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&applied)["ok"], true);
    assert_eq!(structured(&applied)["applied"], true);
    assert_eq!(structured(&applied)["apply_mode"], "checkpoint_restart");

    let status = call_tool(
        &mut server,
        6,
        "run_status",
        json!({
            "run_id": "run-update"
        }),
    );
    assert!(
        structured(&status)["context"]["latest_flow_lock_application"]
            .as_str()
            .expect("application id should be present")
            .starts_with("flow-lock-application:")
    );
}

#[test]
fn apply_flow_update_enforces_required_review_status() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-update-review",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let proposed = call_tool(
        &mut server,
        2,
        "propose_flow_update",
        json!({
            "flow": valid_flow()
        }),
    );
    assert_eq!(structured(&proposed)["ok"], true);
    assert_eq!(structured(&proposed)["review_required"], true);
    let lock_id = structured(&proposed)["flow_lock_id"]
        .as_str()
        .expect("proposal should include lock")
        .to_string();
    let content_hash = structured(&proposed)["content_hash"]
        .as_str()
        .expect("proposal should include hash")
        .to_string();

    let missing_review = call_tool(
        &mut server,
        3,
        "apply_flow_update",
        json!({
            "run_id": "run-update-review",
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(missing_review["result"]["isError"], true);
    assert_eq!(structured(&missing_review)["ok"], false);
    assert_eq!(structured(&missing_review)["review_status"], "missing");
    assert_eq!(
        structured(&missing_review)["error"],
        "flow update review required"
    );
    assert_eq!(
        structured(&missing_review)["next_tool"],
        "prepare_flow_review"
    );

    let prepared = call_tool(
        &mut server,
        4,
        "prepare_flow_review",
        json!({
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&prepared)["review_status"], "pending");
    let review_id = structured(&prepared)["review_id"]
        .as_str()
        .expect("review should include id");

    let pending_review = call_tool(
        &mut server,
        5,
        "apply_flow_update",
        json!({
            "run_id": "run-update-review",
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(pending_review["result"]["isError"], true);
    assert_eq!(structured(&pending_review)["review_status"], "pending");

    let rejected = call_tool(
        &mut server,
        6,
        "approve_flow_review",
        json!({
            "review_id": review_id,
            "decision": "rejected"
        }),
    );
    assert_eq!(structured(&rejected)["review_status"], "rejected");

    let rejected_review = call_tool(
        &mut server,
        7,
        "apply_flow_update",
        json!({
            "run_id": "run-update-review",
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(rejected_review["result"]["isError"], true);
    assert_eq!(structured(&rejected_review)["review_status"], "rejected");
    assert_eq!(
        structured(&rejected_review)["error"],
        "flow update review rejected"
    );
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
fn preview_flow_routes_missing_run_returns_start_run_guidance() {
    let mut server = McpServer::new();

    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);

    let preview = call_tool(
        &mut server,
        2,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-missing",
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );

    assert_tool_error(&preview);
    assert!(preview.get("error").is_none());
    assert_eq!(structured(&preview)["run_id"], "run-preview-missing");
    assert_eq!(structured(&preview)["error"], "run not found");
    assert_eq!(structured(&preview)["next_tool"], "start_run");
    assert_eq!(
        structured(&preview)["next_arguments"],
        json!({
            "run_id": "run-preview-missing",
            "nodes": ["root"]
        })
    );
}

#[test]
fn preview_flow_routes_missing_run_without_explicit_lock_requires_lock_binding() {
    let mut server = McpServer::new();

    let preview = call_tool(
        &mut server,
        1,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-missing-no-lock"
        }),
    );

    assert_tool_error(&preview);
    assert!(preview.get("error").is_none());
    assert_eq!(
        structured(&preview)["run_id"],
        "run-preview-missing-no-lock"
    );
    assert_eq!(structured(&preview)["error"], "flow_lock_id is required");
    assert_eq!(structured(&preview)["next_tool"], "start_run");
    assert_eq!(
        structured(&preview)["next_arguments"],
        json!({
            "run_id": "run-preview-missing-no-lock",
            "nodes": ["root"]
        })
    );
    assert_eq!(structured(&preview)["after_next_tool"], "apply_flow_lock");
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
