mod support;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::mcp::{McpServer, McpSurface};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::json;

use support::mcp::{
    RecordingRunner, assert_prefixed_hex, assert_tool_error, blank_inline_readme_flow, call_tool,
    diagnostic_codes, missing_readme_flow, node_less_missing_readme_flow, readme_resource,
    structured, valid_flow,
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
fn flow_suggest_schema_requires_agent_authored_readme() {
    let surface = McpSurface;
    let descriptor = surface
        .lookup("flow_suggest")
        .expect("flow_suggest descriptor should be present");
    let schema = descriptor.input_schema();

    assert_eq!(schema["required"], json!(["goal", "readme"]));
    assert_eq!(schema["properties"]["goal"]["type"], "string");
    assert_eq!(schema["properties"]["readme"]["type"], "string");
    assert_eq!(schema["properties"]["readme"]["minLength"], 1);
    assert_eq!(schema["properties"]["artifact"]["type"], "string");
    assert_eq!(schema["properties"]["nodes"]["type"], "array");
    assert_eq!(schema["properties"]["nodes"]["items"]["type"], "string");
}

#[test]
fn flow_suggest_rejects_missing_readme_instead_of_generating_one() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_suggest",
        json!({
            "goal": "Draft a concise migration brief."
        }),
    );

    assert_eq!(response["error"]["code"], -32602);
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("readme")
    );
}

#[test]
fn flow_suggest_preserves_readme_verbatim_as_package_root_file() {
    let mut server = isolated_server();
    let readme = "# Audit flow\n\nKeep  two spaces.\n";

    let response = call_tool(
        &mut server,
        1,
        "flow_suggest",
        json!({
            "goal": "Draft a concise migration brief.",
            "readme": readme
        }),
    );

    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        structured(&response)["flow"]["resources"][0],
        json!({
            "path": "README.md",
            "kind": "readme",
            "content": readme
        })
    );
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
            "readme": "Draft a concise migration brief.",
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
                "action": {
                    "driver": "agent",
                    "prompt_ref": "prompts/collect_facts.md",
                    "resource_refs": ["README.md"],
                    "reads": [],
                    "writes": ["artifact.brief"]
                },
                "write_scopes": [],
                "extensions": []
            },
            {
                "id": "review_output",
                "contract_id": "contract.review_output",
                "action": {
                    "driver": "agent",
                    "prompt_ref": "prompts/review_output.md",
                    "resource_refs": ["README.md"],
                    "reads": [],
                    "writes": ["artifact.brief"]
                },
                "write_scopes": [],
                "extensions": []
            }
        ])
    );
    assert_eq!(
        structured(&suggested)["flow"]["contracts"][0],
        json!({
            "id": "contract.collect_facts",
            "completion": "all_artifacts",
            "artifacts": [
                {
                    "id": "brief",
                    "schema_resource_id": "schemas/collect_facts/brief.txt"
                }
            ]
        })
    );
    assert_eq!(
        structured(&suggested)["flow"]["resources"][0],
        json!({
            "path": "README.md",
            "kind": "readme",
            "content": "Draft a concise migration brief."
        })
    );
    assert_eq!(
        structured(&suggested)["flow"]["resources"][3],
        json!({
            "path": "prompts/collect_facts.md",
            "kind": "prompt",
            "content": "Run node collect_facts for goal: Draft a concise migration brief. Deliver artifact with artifact_key \"brief\"."
        })
    );
    assert_eq!(
        structured(&suggested)["flow"]["resources"][4],
        json!({
            "path": "prompts/review_output.md",
            "kind": "prompt",
            "content": "Run node review_output for goal: Draft a concise migration brief. Deliver artifact with artifact_key \"brief\"."
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
                            "promptRef": "prompts/review.md",
                            "resourceRefs": ["scripts/collect.sh"],
                            "reads": ["artifact.handoff", "board.ready"],
                            "writes": ["artifact.summary"],
                            "verdictArtifact": "artifact.review_verdict"
                        }
                    }
                ],
                "resources": [
                    readme_resource(),
                    {
                        "path": "prompts/review.md",
                        "kind": "prompt",
                        "content": "Review the facts."
                    },
                    {
                        "path": "scripts/collect.sh",
                        "kind": "script",
                        "content": "collect"
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
                            "promptRef": "prompts/review.md",
                            "resourceRefs": ["scripts/collect.sh"],
                            "reads": ["artifact.handoff", "board.ready"],
                            "writes": ["artifact.summary"],
                            "verdictArtifact": "artifact.review_verdict"
                        }
                    }
                ],
                "resources": [
                    readme_resource(),
                    {
                        "path": "prompts/review.md",
                        "kind": "prompt",
                        "content": "Review the facts."
                    },
                    {
                        "path": "scripts/collect.sh",
                        "kind": "script",
                        "content": "collect"
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
    let flow = &exported_json["flow"];
    assert_eq!(
        flow["nodes"][0]["action"]["prompt_ref"],
        "prompts/review.md"
    );
    assert_eq!(
        flow["nodes"][0]["action"]["resource_refs"],
        json!(["scripts/collect.sh"])
    );
    assert_eq!(
        flow["nodes"][0]["action"]["verdict_artifact"],
        "artifact.review_verdict"
    );
    assert!(exported_json.get("content").is_none());
}
#[test]
fn flow_repair_returns_no_candidates_for_valid_typed_flows() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_repair",
        json!({
            "flow": valid_flow()
        }),
    );

    assert_eq!(response["result"]["isError"], false);
    assert_eq!(structured(&response)["ok"], true);
    assert_eq!(structured(&response)["repairable"], false);
    assert_eq!(structured(&response)["input_severity"], "none");
    assert_eq!(
        structured(&response)
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "candidates",
            "diagnostics",
            "guidance",
            "input_severity",
            "ok",
            "repairable",
        ])
    );
    assert_eq!(structured(&response)["candidates"], json!([]));
}

#[test]
fn flow_repair_returns_unranked_local_route_target_candidates() {
    let mut server = isolated_server();
    let response = call_tool(
        &mut server,
        1,
        "flow_repair",
        json!({
            "flow": {
                "nodes": [{"id": "zeta"}, {"id": "alpha"}],
                "resources": [readme_resource()],
                "routes": [{
                    "predicate": {
                        "op": "exists",
                        "fact": {"kind": "artifact", "key": "ready"}
                    },
                    "activate": "missing"
                }]
            }
        }),
    );

    assert_eq!(response["result"]["isError"], false);
    assert_eq!(structured(&response)["repairable"], true);
    assert!(structured(&response).get("patches").is_none());
    assert_eq!(
        structured(&response)["candidates"],
        json!([
            {
                "repair_kind": "add_route_target",
                "location": "routes[0].activate",
                "replacement": "zeta"
            },
            {
                "repair_kind": "add_route_target",
                "location": "routes[0].activate",
                "replacement": "alpha"
            }
        ])
    );
}

#[test]
fn flow_repair_warning_guidance_is_opt_in() {
    let mut server = isolated_server();
    let flow = json!({
        "nodes": [{"id": "root"}],
        "resources": [readme_resource()],
        "policies": {"write_scopes": ["workspace"]}
    });

    let default_response = call_tool(&mut server, 1, "flow_repair", json!({"flow": flow.clone()}));
    let included_response = call_tool(
        &mut server,
        2,
        "flow_repair",
        json!({"flow": flow, "include_warnings": true}),
    );

    assert_eq!(structured(&default_response)["diagnostics"], json!([]));
    assert_eq!(structured(&default_response)["guidance"], json!([]));
    assert_eq!(
        diagnostic_codes(&included_response),
        vec!["FLOW_BROAD_WRITE_SCOPE"]
    );
    assert_eq!(structured(&included_response)["repairable"], false);
    assert_eq!(structured(&included_response)["candidates"], json!([]));
}

#[test]
fn flow_repair_keeps_fatal_input_candidate_free() {
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
            }
        }),
    );

    assert_eq!(response["result"]["isError"], false);
    assert_eq!(structured(&response)["ok"], true);
    assert_eq!(structured(&response)["repairable"], false);
    assert_eq!(structured(&response)["input_severity"], "fatal");
    assert!(structured(&response).get("patches").is_none());
    assert_eq!(structured(&response)["candidates"], json!([]));
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
    assert!(structured(&proposed).get("review_required").is_none());
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
        "sha256:",
    );
    let package = &structured(&proposed)["revision_package"];
    assert_eq!(package["lock_id"], structured(&proposed)["flow_lock_id"]);
    assert_eq!(
        package["content_hash"],
        structured(&proposed)["content_hash"]
    );
    assert_eq!(package["format"], "humanize.flow_lock.v1");
    assert!(package["flow"].is_object());
    assert!(package.get("bytes").is_none());
    assert!(package.get("json").is_none());
    assert!(structured(&proposed).get("run_id").is_none());
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
            "readme": "Draft a concise migration brief.",
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
        "sha256:",
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
    assert!(document.contains("README.md"));
}
#[test]
fn flow_suggest_rejects_blank_goal() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_suggest",
        json!({
            "goal": " \t\n ",
            "readme": "Explicit package description."
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
fn flow_check_rejects_string_predicate_at_the_typed_boundary() {
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

    assert_eq!(response["error"]["code"], -32602);
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
fn flow_apply_rejects_string_predicate_at_the_typed_boundary() {
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

    assert_eq!(response["error"]["code"], -32602);
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
                        "predicate": {
                            "op": "exists",
                            "fact": {"kind": "artifact", "key": "ready"}
                        },
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
            .starts_with("sha256:")
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
            .contains("README.md")
    );
}
