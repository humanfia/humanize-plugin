mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::mcp::{McpServer, McpSurface};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::json;

use support::mcp::{
    RecordingRunner, assert_prefixed_hex, assert_tool_error, blank_inline_readme_flow, call_tool,
    diagnostic_codes, lock_valid_flow, missing_readme_flow, node_less_missing_readme_flow,
    readme_resource, structured, valid_flow,
};

static NEXT_ASSET_ROOT: AtomicU64 = AtomicU64::new(1);

fn isolated_server() -> McpServer<RecordingRunner> {
    let index = NEXT_ASSET_ROOT.fetch_add(1, Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("mcp-authoring-assets-{index}"));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    McpServer::with_tmux_runner_and_run_asset_store(
        RecordingRunner::default(),
        RunAssetStore::new(RunAssetSink::Root(root)),
    )
}

#[test]
fn flow_suggest_schema_covers_goal_nodes_and_artifact() {
    let surface = McpSurface;
    let descriptor = surface
        .lookup("flow_suggest")
        .expect("flow_suggest descriptor should be present");
    let schema = descriptor.input_schema();

    assert_eq!(schema["required"], json!(["goal"]));
    assert_eq!(schema["properties"]["goal"]["type"], "string");
    assert_eq!(schema["properties"]["artifact"]["type"], "string");
    assert_eq!(schema["properties"]["nodes"]["type"], "array");
    assert_eq!(schema["properties"]["nodes"]["items"]["type"], "string");
}
#[test]
fn flow_suggest_returns_valid_draft_accepted_by_flow_check() {
    let mut server = isolated_server();

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

    assert_eq!(suggested["result"]["isError"], false);
    assert_eq!(structured(&suggested)["ok"], true);
    assert_eq!(structured(&suggested)["valid"], true);
    assert_eq!(structured(&suggested)["mode"], "core");
    assert_eq!(structured(&suggested)["diagnostics"], json!([]));
    assert_eq!(
        structured(&suggested)["flow"]["nodes"],
        json!([
            {
                "id": "collect_facts",
                "contract_id": "contract.collect_facts",
                "write_scopes": [],
                "extensions": []
            },
            {
                "id": "review_output",
                "contract_id": "contract.review_output",
                "write_scopes": [],
                "extensions": []
            }
        ])
    );
    assert!(
        structured(&suggested)["flow"]["nodes"][0]
            .get("action")
            .is_none()
    );
    assert_eq!(
        structured(&suggested)["flow"]["contracts"][0],
        json!({
            "id": "contract.collect_facts",
            "completion": "all_artifacts",
            "artifacts": [
                {
                    "id": "brief",
                    "schema_resource_id": "schema.collect_facts.brief"
                }
            ]
        })
    );
    assert_eq!(
        structured(&suggested)["flow"]["resources"][0],
        json!({
            "id": "readme.main",
            "kind": "readme",
            "source": "inline:Draft a concise migration brief."
        })
    );
    assert_eq!(structured(&suggested)["flow"]["routes"], json!([]));
    assert_eq!(structured(&suggested)["flow"]["imports"], json!([]));
    assert_eq!(
        structured(&suggested)["flow"]["policies"],
        json!({ "write_scopes": [] })
    );
    assert_eq!(structured(&suggested)["flow"]["extensions"], json!([]));

    let checked = call_tool(
        &mut server,
        2,
        "flow_check",
        json!({
            "flow": structured(&suggested)["flow"].clone()
        }),
    );

    assert_eq!(checked["result"]["isError"], false);
    assert_eq!(structured(&checked)["ok"], true);
    assert_eq!(structured(&checked)["diagnostics"], json!([]));
}

#[test]
fn flow_check_accepts_node_action_descriptor() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_check",
        json!({
            "mode": "core",
            "flow": {
                "nodes": [
                    {
                        "id": "root",
                        "action": {
                            "driver": "agent",
                            "promptRef": "prompt.review",
                            "resourceRefs": ["script.collect"],
                            "reads": ["artifact.handoff", "board.ready"],
                            "writes": ["artifact.summary"],
                            "verdictArtifact": "artifact.review_verdict"
                        }
                    }
                ],
                "resources": [
                    readme_resource(),
                    {
                        "id": "prompt.review",
                        "kind": "prompt",
                        "source": "inline:Review the facts."
                    },
                    {
                        "id": "script.collect",
                        "kind": "script",
                        "source": "scripts/collect.sh"
                    }
                ]
            }
        }),
    );

    assert_eq!(response["result"]["isError"], false);
    assert_eq!(structured(&response)["ok"], true);
    assert_eq!(structured(&response)["diagnostics"], json!([]));
}

#[test]
fn flow_action_export_uses_snake_case_after_lock() {
    let mut server = isolated_server();

    let locked = call_tool(
        &mut server,
        1,
        "flow_lock",
        json!({
            "mode": "core",
            "flow": {
                "nodes": [
                    {
                        "id": "root",
                        "action": {
                            "driver": "review",
                            "promptRef": "prompt.review",
                            "resourceRefs": ["script.collect"],
                            "reads": ["artifact.handoff", "board.ready"],
                            "writes": ["artifact.summary"],
                            "verdictArtifact": "artifact.review_verdict"
                        }
                    }
                ],
                "resources": [
                    readme_resource(),
                    {
                        "id": "prompt.review",
                        "kind": "prompt",
                        "source": "inline:Review the facts."
                    },
                    {
                        "id": "script.collect",
                        "kind": "script",
                        "source": "scripts/collect.sh"
                    }
                ]
            }
        }),
    );

    assert_eq!(locked["result"]["isError"], false);
    assert_eq!(structured(&locked)["ok"], true);
    let lock_id = structured(&locked)["flow_lock_id"]
        .as_str()
        .expect("flow_lock should return a flow lock id");

    let exported = call_tool(
        &mut server,
        2,
        "flow_export",
        json!({
            "flow_lock_id": lock_id,
            "format": "json"
        }),
    );

    assert_eq!(exported["result"]["isError"], false);
    assert_eq!(structured(&exported)["ok"], true);
    let document = structured(&exported)["document"]
        .as_str()
        .expect("export should include a document");
    let exported_json = serde_json::from_str::<serde_json::Value>(document)
        .expect("exported document should be JSON");
    let content = exported_json["content"]
        .as_str()
        .expect("exported document should include normalized content");

    assert!(content.contains("\"prompt_ref\":\"prompt.review\""));
    assert!(content.contains("\"resource_refs\":[\"script.collect\"]"));
    assert!(content.contains("\"verdict_artifact\":\"artifact.review_verdict\""));
    assert!(!content.contains("promptRef"));
    assert!(!content.contains("resourceRefs"));
    assert!(!content.contains("verdictArtifact"));
}
#[test]
fn flow_repair_returns_mechanical_patches_and_unranked_candidates() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_repair",
        json!({
            "flow": valid_flow(),
            "route_authoring": [
                {
                    "when": "exists(artifact.ready)",
                    "to": "finish"
                },
                {
                    "predicate": {
                        "artifact": "summary"
                    },
                    "activate": "finish"
                },
                {
                    "predicate": "artifact.report.delivered",
                    "activate": ""
                }
            ]
        }),
    );

    assert_eq!(response["result"]["isError"], false);
    assert_eq!(structured(&response)["ok"], true);
    assert_eq!(structured(&response)["repairable"], true);
    assert_eq!(structured(&response)["input_severity"], "none");
    assert_eq!(
        structured(&response)["patches"][0],
        json!({
            "repair_kind": "route_when_to_predicate",
            "location": "routes[0].when",
            "replacement": "predicate: exists(artifact.ready)"
        })
    );
    let candidate = structured(&response)["candidates"][0]
        .as_object()
        .expect("candidate should be an object");
    assert_eq!(
        candidate.get("repair_kind"),
        Some(&json!("route_bare_artifact_delivered_to_exists"))
    );
    assert!(candidate.get("rank").is_none());
    assert!(candidate.get("recommended").is_none());
    assert!(candidate.get("best_candidate").is_none());
}

#[test]
fn flow_repair_keeps_fatal_input_patch_free() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_repair",
        json!({
            "flow": {
                "nodes": ["root"],
                "resources": [readme_resource()],
                "extensions": ["NodeActivation"]
            },
            "route_authoring": [
                {
                    "when": "exists(artifact.ready)",
                    "to": "root"
                }
            ]
        }),
    );

    assert_eq!(response["result"]["isError"], false);
    assert_eq!(structured(&response)["ok"], true);
    assert_eq!(structured(&response)["repairable"], false);
    assert_eq!(structured(&response)["input_severity"], "fatal");
    assert_eq!(structured(&response)["patches"], json!([]));
    assert_eq!(
        diagnostic_codes(&response),
        vec!["FLOW_AUTHORING_PRIMITIVE_MISUSE"]
    );
}

#[test]
fn propose_flow_update_locks_flow_and_reports_review_risk() {
    let mut server = isolated_server();

    let proposed = call_tool(
        &mut server,
        1,
        "propose_flow_update",
        json!({
            "flow": valid_flow(),
            "apply_mode": "future_activations",
            "summary": "Add reviewed route activation."
        }),
    );

    assert_eq!(proposed["result"]["isError"], false);
    assert_eq!(structured(&proposed)["ok"], true);
    assert_eq!(structured(&proposed)["apply_mode"], "future_activations");
    assert_eq!(structured(&proposed)["review_required"], true);
    assert_eq!(structured(&proposed)["risk"], "medium");
    assert_prefixed_hex(
        structured(&proposed)["flow_lock_id"]
            .as_str()
            .expect("proposal should include a flow lock id"),
        "flk_",
    );
    assert_prefixed_hex(
        structured(&proposed)["content_hash"]
            .as_str()
            .expect("proposal should include a content hash"),
        "fnv1a64:",
    );
}

#[test]
fn flow_check_rejects_unknown_action_driver_at_parse_time() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_check",
        json!({
            "mode": "core",
            "flow": {
                "nodes": [
                    {
                        "id": "root",
                        "action": {
                            "driver": "worker",
                            "reads": ["artifact.handoff"],
                            "writes": ["artifact.summary"]
                        }
                    }
                ],
                "resources": [readme_resource()]
            }
        }),
    );

    assert_eq!(response["error"]["code"], -32602);
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("unknown action driver")
    );
}

#[test]
fn flow_suggest_flow_round_trips_through_lock_and_export() {
    let mut server = isolated_server();

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

    assert_eq!(structured(&suggested)["ok"], true);
    let flow = structured(&suggested)["flow"].clone();

    let locked = call_tool(
        &mut server,
        2,
        "flow_lock",
        json!({
            "flow": flow
        }),
    );

    assert_eq!(locked["result"]["isError"], false);
    assert_eq!(structured(&locked)["ok"], true);
    assert_eq!(structured(&locked)["mode"], "core");
    let lock_id = structured(&locked)["flow_lock_id"]
        .as_str()
        .expect("flow_lock should return a flow lock id");
    assert_eq!(structured(&locked)["lock_id"], lock_id);
    assert_prefixed_hex(lock_id, "flk_");
    assert_prefixed_hex(
        structured(&locked)["content_hash"]
            .as_str()
            .expect("flow_lock should return a content hash"),
        "fnv1a64:",
    );

    let exported = call_tool(
        &mut server,
        3,
        "flow_export",
        json!({
            "flow_lock_id": lock_id,
            "format": "json"
        }),
    );

    assert_eq!(exported["result"]["isError"], false);
    assert_eq!(structured(&exported)["ok"], true);
    assert_eq!(structured(&exported)["flow_lock_id"], lock_id);
    let document = structured(&exported)["document"]
        .as_str()
        .expect("export should include a document");
    assert!(document.contains(lock_id));
    assert!(document.contains("readme.main"));
}
#[test]
fn flow_suggest_rejects_blank_goal() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_suggest",
        json!({
            "goal": " \t\n "
        }),
    );

    assert_eq!(response["error"]["code"], -32602);
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("goal")
    );
}
#[test]
fn flow_check_rejects_effectful_predicate_diagnostics() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_check",
        json!({
            "mode": "core",
            "flow": {
                "nodes": [
                    { "id": "start" },
                    { "id": "finish" }
                ],
                "resources": [readme_resource()],
                "routes": [
                    {
                        "predicate": "shell('cargo test')",
                        "activate": "finish"
                    }
                ]
            }
        }),
    );

    assert_tool_error(&response);
    assert_eq!(
        diagnostic_codes(&response),
        vec!["FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN"]
    );
}
#[test]
fn flow_check_rejects_missing_readme_in_core_and_strict() {
    for (id, mode) in [(1, "core"), (2, "strict")] {
        let mut server = isolated_server();

        let response = call_tool(
            &mut server,
            id,
            "flow_check",
            json!({
                "mode": mode,
                "flow": missing_readme_flow()
            }),
        );

        assert_tool_error(&response);
        assert_eq!(structured(&response)["mode"], mode);
        assert_eq!(diagnostic_codes(&response), vec!["FLOW_MISSING_README"]);
        assert_eq!(structured(&response)["diagnostics"][0]["severity"], "error");
    }
}
#[test]
fn flow_check_rejects_node_less_non_empty_flow_missing_readme() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_check",
        json!({
            "mode": "core",
            "flow": node_less_missing_readme_flow()
        }),
    );

    assert_tool_error(&response);
    assert_eq!(diagnostic_codes(&response), vec!["FLOW_MISSING_README"]);
}
#[test]
fn flow_check_rejects_blank_inline_readme() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_check",
        json!({
            "mode": "core",
            "flow": blank_inline_readme_flow()
        }),
    );

    assert_tool_error(&response);
    assert_eq!(diagnostic_codes(&response), vec!["FLOW_EMPTY_README"]);
    assert_eq!(structured(&response)["diagnostics"][0]["severity"], "error");
}
#[test]
fn flow_check_keeps_core_warning_diagnostics_successful() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_check",
        json!({
            "mode": "core",
            "flow": {
                "nodes": ["root"],
                "resources": [readme_resource()],
                "policies": {
                    "write_scopes": ["workspace"]
                }
            }
        }),
    );

    assert_eq!(response["result"]["isError"], false);
    assert_eq!(structured(&response)["ok"], true);
    assert_eq!(diagnostic_codes(&response), vec!["FLOW_BROAD_WRITE_SCOPE"]);
    assert_eq!(
        structured(&response)["diagnostics"][0]["severity"],
        "warning"
    );
}
#[test]
fn flow_lock_rejects_missing_readme() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_lock",
        json!({
            "mode": "core",
            "flow": missing_readme_flow()
        }),
    );

    assert_tool_error(&response);
    assert_eq!(structured(&response)["mode"], "core");
    assert_eq!(diagnostic_codes(&response), vec!["FLOW_MISSING_README"]);
    assert_eq!(structured(&response)["diagnostics"][0]["severity"], "error");
}
#[test]
fn flow_lock_rejects_node_less_non_empty_flow_missing_readme() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_lock",
        json!({
            "mode": "core",
            "flow": node_less_missing_readme_flow()
        }),
    );

    assert_tool_error(&response);
    assert_eq!(diagnostic_codes(&response), vec!["FLOW_MISSING_README"]);
}
#[test]
fn flow_apply_rejects_missing_readme() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_apply",
        json!({
            "flow": missing_readme_flow()
        }),
    );

    assert_tool_error(&response);
    assert_eq!(structured(&response)["mode"], "core");
    assert_eq!(diagnostic_codes(&response), vec!["FLOW_MISSING_README"]);
    assert_eq!(structured(&response)["diagnostics"][0]["severity"], "error");
}
#[test]
fn flow_apply_rejects_node_less_non_empty_flow_missing_readme() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_apply",
        json!({
            "flow": node_less_missing_readme_flow()
        }),
    );

    assert_tool_error(&response);
    assert_eq!(structured(&response)["mode"], "core");
    assert_eq!(diagnostic_codes(&response), vec!["FLOW_MISSING_README"]);
}
#[test]
fn flow_apply_rejects_empty_and_non_object_flows() {
    let mut server = isolated_server();

    let empty = call_tool(
        &mut server,
        1,
        "flow_apply",
        json!({
            "flow": {}
        }),
    );
    assert_eq!(empty["error"]["code"], -32602);
    assert!(
        empty["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("flow")
    );

    let non_object = call_tool(
        &mut server,
        2,
        "flow_apply",
        json!({
            "flow": []
        }),
    );
    assert_eq!(non_object["error"]["code"], -32602);
    assert!(
        non_object["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("flow")
    );
}
#[test]
fn flow_apply_rejects_effectful_predicate_with_diagnostics() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_apply",
        json!({
            "flow": {
                "nodes": [
                    { "id": "start" },
                    { "id": "finish" }
                ],
                "resources": [readme_resource()],
                "routes": [
                    {
                        "predicate": "shell('cargo test')",
                        "activate": "finish"
                    }
                ]
            }
        }),
    );

    assert_eq!(response["result"]["isError"], true);
    assert_eq!(structured(&response)["ok"], false);
    assert_eq!(
        diagnostic_codes(&response),
        vec!["FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN"]
    );
}
#[test]
fn flow_apply_records_valid_flow_lock_for_export() {
    let mut server = isolated_server();

    let applied = call_tool(
        &mut server,
        1,
        "flow_apply",
        json!({
            "flow": {
                "nodes": [
                    { "id": "start" },
                    { "id": "finish" }
                ],
                "resources": [readme_resource()],
                "routes": [
                    {
                        "predicate": "exists(artifact.ready)",
                        "activate": "finish"
                    }
                ]
            }
        }),
    );

    assert_eq!(structured(&applied)["ok"], true);
    assert_eq!(structured(&applied)["mode"], "core");
    let lock_id = structured(&applied)["flow_lock_id"]
        .as_str()
        .expect("flow_apply should return a flow lock id");
    assert!(lock_id.starts_with("flk_"));
    assert!(
        structured(&applied)["content_hash"]
            .as_str()
            .expect("flow_apply should return content hash")
            .starts_with("fnv1a64:")
    );

    let exported = call_tool(
        &mut server,
        2,
        "flow_export",
        json!({
            "flow_lock_id": lock_id,
            "format": "json"
        }),
    );
    assert_eq!(structured(&exported)["ok"], true);
    assert!(
        structured(&exported)["document"]
            .as_str()
            .expect("export should include a document")
            .contains(lock_id)
    );
    assert!(
        structured(&exported)["document"]
            .as_str()
            .expect("export should include a document")
            .contains("readme.main")
    );
}
#[test]
fn apply_flow_lock_requires_and_records_lock_provenance() {
    let mut server = isolated_server();

    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);

    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-lock",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let missing_provenance = call_tool(
        &mut server,
        3,
        "apply_flow_lock",
        json!({
            "run_id": "run-lock",
            "mode": "future_activations",
            "content_hash": content_hash
        }),
    );
    assert_eq!(missing_provenance["error"]["code"], -32602);
    assert!(
        missing_provenance["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("lock_id")
    );

    let applied = call_tool(
        &mut server,
        4,
        "apply_flow_lock",
        json!({
            "run_id": "run-lock",
            "mode": "future_activations",
            "lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&applied)["ok"], true);
    assert_eq!(structured(&applied)["lock_id"], lock_id);
    assert_eq!(structured(&applied)["content_hash"], content_hash);

    let context = call_tool(
        &mut server,
        5,
        "get_context",
        json!({
            "run_id": "run-lock"
        }),
    );
    let applications = structured(&context)["context"]["flow_lock_applications"]
        .as_object()
        .expect("flow lock applications should be exported from runtime state");
    let latest = applications
        .values()
        .next()
        .expect("one flow lock application should be recorded");
    assert_eq!(latest["lock_id"], lock_id);
    assert_eq!(latest["content_hash"], content_hash);
}
#[test]
fn apply_flow_lock_rejects_unknown_lock_without_runtime_apply() {
    let mut server = isolated_server();

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-unknown-lock",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let response = call_tool(
        &mut server,
        2,
        "apply_flow_lock",
        json!({
            "run_id": "run-unknown-lock",
            "mode": "future_activations",
            "lock_id": "lock-missing",
            "content_hash": "fnv1a64:0000000000000000"
        }),
    );

    assert_tool_error(&response);
    assert_eq!(structured(&response)["lock_id"], "lock-missing");
    assert_eq!(structured(&response)["error"], "flow lock not found");

    let context = call_tool(
        &mut server,
        3,
        "get_context",
        json!({
            "run_id": "run-unknown-lock"
        }),
    );
    assert!(structured(&context)["context"]["flow_lock_mode"].is_null());
    assert_eq!(
        structured(&context)["context"]["flow_lock_applications"],
        json!({})
    );
}
#[test]
fn apply_flow_lock_rejects_hash_mismatch_without_runtime_apply() {
    let mut server = isolated_server();

    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);

    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-hash-mismatch",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let response = call_tool(
        &mut server,
        3,
        "apply_flow_lock",
        json!({
            "run_id": "run-hash-mismatch",
            "mode": "future_activations",
            "lock_id": lock_id,
            "content_hash": "fnv1a64:0000000000000000"
        }),
    );

    assert_tool_error(&response);
    assert_eq!(structured(&response)["lock_id"], lock_id);
    assert_eq!(
        structured(&response)["content_hash"],
        "fnv1a64:0000000000000000"
    );
    assert_eq!(structured(&response)["expected_content_hash"], content_hash);
    assert_eq!(
        structured(&response)["error"],
        "flow lock content hash mismatch"
    );

    let context = call_tool(
        &mut server,
        4,
        "get_context",
        json!({
            "run_id": "run-hash-mismatch"
        }),
    );
    assert!(structured(&context)["context"]["flow_lock_mode"].is_null());
    assert_eq!(
        structured(&context)["context"]["flow_lock_applications"],
        json!({})
    );
}

#[test]
fn apply_flow_lock_missing_run_returns_start_run_guidance() {
    let mut server = isolated_server();

    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);

    let response = call_tool(
        &mut server,
        2,
        "apply_flow_lock",
        json!({
            "run_id": "run-apply-missing",
            "mode": "future_activations",
            "lock_id": lock_id,
            "content_hash": content_hash
        }),
    );

    assert_tool_error(&response);
    assert!(response.get("error").is_none());
    assert_eq!(structured(&response)["run_id"], "run-apply-missing");
    assert_eq!(structured(&response)["mode"], "future_activations");
    assert_eq!(structured(&response)["lock_id"], lock_id);
    assert_eq!(structured(&response)["flow_lock_id"], lock_id);
    assert_eq!(structured(&response)["content_hash"], content_hash);
    assert_eq!(structured(&response)["error"], "run not found");
    assert_eq!(structured(&response)["next_tool"], "start_run");
    assert_eq!(
        structured(&response)["next_arguments"],
        json!({
            "run_id": "run-apply-missing",
            "nodes": ["root"]
        })
    );
}

#[test]
fn apply_flow_lock_unknown_lock_takes_precedence_over_missing_run() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "apply_flow_lock",
        json!({
            "run_id": "run-missing-unknown-lock",
            "mode": "future_activations",
            "lock_id": "lock-missing",
            "content_hash": "fnv1a64:0000000000000000"
        }),
    );

    assert_tool_error(&response);
    assert!(response.get("error").is_none());
    assert_eq!(structured(&response)["run_id"], "run-missing-unknown-lock");
    assert_eq!(structured(&response)["lock_id"], "lock-missing");
    assert_eq!(structured(&response)["flow_lock_id"], "lock-missing");
    assert_eq!(structured(&response)["error"], "flow lock not found");
    assert!(structured(&response)["next_tool"].is_null());
    assert!(structured(&response)["next_arguments"].is_null());
}

#[test]
fn apply_flow_lock_hash_mismatch_takes_precedence_over_missing_run() {
    let mut server = isolated_server();

    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);

    let response = call_tool(
        &mut server,
        2,
        "apply_flow_lock",
        json!({
            "run_id": "run-missing-hash-mismatch",
            "mode": "future_activations",
            "flow_lock_id": lock_id,
            "content_hash": "fnv1a64:0000000000000000"
        }),
    );

    assert_tool_error(&response);
    assert!(response.get("error").is_none());
    assert_eq!(structured(&response)["run_id"], "run-missing-hash-mismatch");
    assert_eq!(structured(&response)["mode"], "future_activations");
    assert_eq!(structured(&response)["lock_id"], lock_id);
    assert_eq!(structured(&response)["flow_lock_id"], lock_id);
    assert_eq!(
        structured(&response)["content_hash"],
        "fnv1a64:0000000000000000"
    );
    assert_eq!(structured(&response)["expected_content_hash"], content_hash);
    assert_eq!(
        structured(&response)["error"],
        "flow lock content hash mismatch"
    );
    assert!(structured(&response)["next_tool"].is_null());
    assert!(structured(&response)["next_arguments"].is_null());
}
#[test]
fn apply_flow_lock_accepts_flow_lock_id_alias_for_stored_lock() {
    let mut server = isolated_server();

    let (lock_id, content_hash) = lock_valid_flow(&mut server, 1);
    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-lock-alias",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let applied = call_tool(
        &mut server,
        3,
        "apply_flow_lock",
        json!({
            "run_id": "run-lock-alias",
            "mode": "checkpoint_restart",
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );

    assert_eq!(structured(&applied)["ok"], true);
    assert_eq!(structured(&applied)["lock_id"], lock_id);
    assert_eq!(structured(&applied)["content_hash"], content_hash);
    assert_eq!(structured(&applied)["mode"], "checkpoint_restart");
}
#[test]
fn apply_flow_lock_rejects_lock_created_in_another_server() {
    let mut authoring_server = isolated_server();
    let (lock_id, content_hash) = lock_valid_flow(&mut authoring_server, 1);

    let mut runtime_server = isolated_server();
    let started = call_tool(
        &mut runtime_server,
        1,
        "start_run",
        json!({
            "run_id": "run-other-server-lock",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let response = call_tool(
        &mut runtime_server,
        2,
        "apply_flow_lock",
        json!({
            "run_id": "run-other-server-lock",
            "mode": "future_activations",
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );

    assert_tool_error(&response);
    assert_eq!(structured(&response)["lock_id"], lock_id);
    assert_eq!(structured(&response)["error"], "flow lock not found");
}
