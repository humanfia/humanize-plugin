mod support;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::mcp::{McpServer, McpSurface};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use support::mcp::{RecordingRunner, call_tool, readme_resource};

static NEXT_STDIO_ASSET_ROOT: AtomicU64 = AtomicU64::new(1);

fn child_asset_root(name: &str) -> PathBuf {
    let index = NEXT_STDIO_ASSET_ROOT.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("humanize-{name}-{}-{index}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    root
}

fn assert_required_alias_group(schema: &Value, aliases: &[&str]) {
    let all_of = schema["allOf"]
        .as_array()
        .expect("schema should express aliased required fields with allOf");
    let found = all_of.iter().any(|entry| {
        let Some(any_of) = entry["anyOf"].as_array() else {
            return false;
        };
        aliases.iter().all(|alias| {
            any_of
                .iter()
                .any(|candidate| candidate["required"] == json!([alias]))
        })
    });

    assert!(found, "schema should require one of {aliases:?}");
}

fn expected_tool_names() -> Vec<&'static str> {
    vec![
        "get_context",
        "deliver_artifact",
        "fanout_from_artifact",
        "record_effect",
        "patch_board",
        "activate_node",
        "send_message",
        "validate_stop",
        "apply_flow_lock",
        "preview_flow_routes",
        "run_flow",
        "run_status",
        "run_why",
        "pause_run",
        "resume_run",
        "complete_run",
        "stop_run",
        "view_terminal",
        "view_snapshot",
        "flow_repair",
        "flow_apply",
        "flow_suggest",
        "flow_check",
        "flow_lock",
        "flow_export",
        "propose_flow_update",
        "apply_flow_update",
        "prepare_flow_review",
        "decide_flow_review",
    ]
}

#[test]
fn mcp_surface_exposes_exact_tool_names_and_lookup() {
    let surface = McpSurface;
    let names: Vec<_> = surface.tools().iter().map(|tool| tool.name()).collect();

    assert_eq!(names, expected_tool_names());
    for name in expected_tool_names() {
        let descriptor = surface.lookup(name).expect("tool should be present");
        assert_eq!(descriptor.name(), name);
    }
    assert!(surface.lookup("unknown_tool").is_none());
}

#[test]
fn unsupported_runtime_tools_are_hidden_from_list_and_lookup() {
    let surface = McpSurface;
    let names = surface
        .tools()
        .into_iter()
        .map(|tool| tool.name())
        .collect::<Vec<_>>();

    for hidden in [
        "start_run",
        "view_browser",
        "record_hook_fact",
        "observe_stop",
    ] {
        assert!(!names.contains(&hidden));
        assert!(surface.lookup(hidden).is_none());
    }
}

#[test]
fn hidden_tools_are_rejected_for_every_runner_without_creating_state() {
    let root = child_asset_root("mcp-hidden-tools");
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));
    let mut server =
        McpServer::with_tmux_runner_and_run_asset_store(RecordingRunner::default(), store.clone());

    for (id, name, arguments) in [
        (
            1,
            "start_run",
            json!({ "run_id": "run-hidden", "nodes": [{ "id": "root" }] }),
        ),
        (
            2,
            "view_browser",
            json!({ "run_id": "run-hidden", "host": "127.0.0.1" }),
        ),
    ] {
        let response = call_tool(&mut server, id, name, arguments);
        assert_eq!(response["error"]["code"], -32602, "{response}");
        assert_eq!(response["error"]["message"], "unknown tool", "{response}");
    }

    assert!(!store.run_root("run-hidden").unwrap().exists());
}

#[test]
fn every_advertised_run_read_requires_run_id() {
    let surface = McpSurface;

    for name in [
        "get_context",
        "run_status",
        "run_why",
        "validate_stop",
        "preview_flow_routes",
        "view_terminal",
        "view_snapshot",
    ] {
        let descriptor = surface.lookup(name).expect("tool should be advertised");
        assert_required_alias_group(descriptor.input_schema(), &["run_id", "runId"]);
    }
}

#[test]
fn participant_message_schema_requires_target_identity_and_text() {
    let descriptor = McpSurface
        .lookup("send_message")
        .expect("send_message should be advertised");
    let schema = descriptor.input_schema();

    assert_required_alias_group(schema, &["run_id", "runId"]);
    assert_required_alias_group(schema, &["activation_id", "activationId"]);
    assert_required_alias_group(schema, &["message_id", "messageId"]);
    assert_required_alias_group(schema, &["text", "message"]);
}

#[test]
fn every_advertised_run_tool_rejects_missing_run_id_before_dispatch() {
    let mut server = McpServer::new();
    let run_tools = [
        "get_context",
        "deliver_artifact",
        "fanout_from_artifact",
        "record_effect",
        "patch_board",
        "activate_node",
        "send_message",
        "validate_stop",
        "apply_flow_lock",
        "preview_flow_routes",
        "run_flow",
        "run_status",
        "run_why",
        "pause_run",
        "resume_run",
        "complete_run",
        "stop_run",
        "view_terminal",
        "view_snapshot",
        "apply_flow_update",
    ];

    for (index, name) in run_tools.into_iter().enumerate() {
        let response = call_tool(&mut server, index as u64 + 100, name, json!({}));
        assert_eq!(response["error"]["code"], -32602, "{name}: {response}");
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("run_id")),
            "{name}: {response}"
        );
    }
}
#[test]
fn fanout_from_artifact_schema_requires_runtime_arguments() {
    let surface = McpSurface;
    let descriptor = surface
        .lookup("fanout_from_artifact")
        .expect("fanout_from_artifact descriptor should be present");
    let schema = descriptor.input_schema();

    assert_eq!(schema["required"], json!([]));
    assert_required_alias_group(schema, &["run_id", "runId"]);
    assert_required_alias_group(schema, &["node_id", "nodeId"]);
    assert_required_alias_group(schema, &["artifact_key", "artifactKey", "key"]);
    assert_eq!(schema["properties"]["run_id"]["type"], "string");
    assert_eq!(schema["properties"]["runId"]["type"], "string");
    assert_eq!(schema["properties"]["node_id"]["type"], "string");
    assert_eq!(schema["properties"]["nodeId"]["type"], "string");
    assert_eq!(schema["properties"]["artifact_key"]["type"], "string");
    assert_eq!(schema["properties"]["artifactKey"]["type"], "string");
    assert_eq!(schema["properties"]["key"]["type"], "string");
    assert_eq!(schema["properties"]["for_each"]["type"], "string");
    assert_eq!(schema["properties"]["forEach"]["type"], "string");
    assert_eq!(schema["properties"]["required_artifacts"]["type"], "array");
    assert_eq!(schema["properties"]["required_effects"]["type"], "array");
}
#[test]
fn preview_flow_routes_schema_requires_only_run_id() {
    let surface = McpSurface;
    let descriptor = surface
        .lookup("preview_flow_routes")
        .expect("preview_flow_routes descriptor should be present");
    let schema = descriptor.input_schema();

    assert_eq!(schema["required"], json!([]));
    assert_required_alias_group(schema, &["run_id", "runId"]);
    assert_eq!(schema["properties"]["run_id"]["type"], "string");
    assert_eq!(schema["properties"]["runId"]["type"], "string");
    assert_eq!(schema["properties"]["flow_lock_id"]["type"], "string");
    assert_eq!(schema["properties"]["flowLockId"]["type"], "string");
    assert_eq!(schema["properties"]["lock_id"]["type"], "string");
    assert_eq!(schema["properties"]["lockId"]["type"], "string");
    assert_eq!(schema["properties"]["content_hash"]["type"], "string");
    assert_eq!(schema["properties"]["contentHash"]["type"], "string");
}
#[test]
fn new_runtime_authoring_and_review_tool_groups_are_explicit() {
    let surface = McpSurface;
    let runtime_names: Vec<_> = surface
        .runtime_tools()
        .iter()
        .map(|tool| tool.name())
        .collect();
    let authoring_names: Vec<_> = surface
        .authoring_tools()
        .iter()
        .map(|tool| tool.name())
        .collect();
    let review_names: Vec<_> = surface
        .review_tools()
        .iter()
        .map(|tool| tool.name())
        .collect();

    for name in [
        "run_flow",
        "run_status",
        "run_why",
        "pause_run",
        "resume_run",
        "complete_run",
        "stop_run",
    ] {
        assert!(runtime_names.contains(&name));
    }
    for name in ["flow_repair", "propose_flow_update"] {
        assert!(authoring_names.contains(&name));
    }
    assert!(runtime_names.contains(&"apply_flow_update"));
    for name in ["prepare_flow_review", "decide_flow_review"] {
        assert!(review_names.contains(&name));
    }
}

#[test]
fn generation_aware_run_controls_are_advertised_with_bounded_schemas() {
    let surface = McpSurface;
    let run_flow_descriptor = surface
        .lookup("run_flow")
        .expect("run_flow should be advertised");
    let run_flow = run_flow_descriptor.input_schema();
    assert_eq!(
        run_flow["properties"]["run_mode"]["enum"],
        json!(["finite", "continuous", "manual"])
    );
    assert_eq!(run_flow["properties"]["activation_limit"]["minimum"], 0);
    assert_eq!(run_flow["properties"]["stop_attempt_limit"]["minimum"], 1);
    assert_eq!(run_flow["properties"]["stop_attempt_limit"]["maximum"], 8);

    let resume_descriptor = surface
        .lookup("resume_run")
        .expect("resume_run should be advertised");
    let resume = resume_descriptor.input_schema();
    assert_eq!(resume["properties"]["activation_limit"]["minimum"], 0);

    let complete_descriptor = surface
        .lookup("complete_run")
        .expect("complete_run should be advertised");
    let complete = complete_descriptor.input_schema();
    assert_required_alias_group(complete, &["run_id", "runId"]);
}

#[test]
fn authoring_tool_descriptions_explain_natural_language_entry_path() {
    let surface = McpSurface;
    let flow_suggest = surface
        .lookup("flow_suggest")
        .expect("flow_suggest descriptor should be present");
    let description = flow_suggest.description();

    assert!(description.contains("Humanize entry"));
    assert!(description.contains("terse natural-language"));
    for name in ["flow_check", "flow_lock", "prepare_flow_review", "run_flow"] {
        assert!(description.contains(name));
    }

    let review = surface
        .lookup("prepare_flow_review")
        .expect("prepare_flow_review descriptor should be present");
    assert!(review.description().contains("human-readable review"));
    assert!(review.description().contains("long-running execution"));
}

#[test]
fn new_tool_schemas_cover_core_arguments() {
    let surface = McpSurface;

    let flow_repair = surface
        .lookup("flow_repair")
        .expect("flow_repair descriptor should be present");
    assert_eq!(flow_repair.input_schema()["required"], json!(["flow"]));
    assert_eq!(
        flow_repair.input_schema()["properties"]["include_warnings"]["type"],
        "boolean"
    );
    assert!(flow_repair.description().contains("does not modify"));
    assert!(flow_repair.description().contains("unranked"));
    assert!(flow_repair.description().contains("authored order"));
    assert!(
        flow_repair
            .description()
            .contains("guidance and diagnostics")
    );
    assert!(
        flow_repair.input_schema()["properties"]
            .get("route_authoring")
            .is_none()
    );

    for name in [
        "run_flow",
        "flow_repair",
        "flow_apply",
        "flow_check",
        "flow_lock",
        "propose_flow_update",
        "prepare_flow_review",
    ] {
        let descriptor = surface.lookup(name).expect("descriptor should be present");
        assert_eq!(
            descriptor.input_schema()["properties"]["flow"]["type"],
            "object",
            "{name} must expose flow as an object"
        );
    }

    let run_flow = surface
        .lookup("run_flow")
        .expect("run_flow descriptor should be present");
    assert_eq!(run_flow.input_schema()["required"], json!([]));
    assert_required_alias_group(run_flow.input_schema(), &["run_id", "runId"]);
    assert_eq!(
        run_flow.input_schema()["properties"]["runId"]["type"],
        "string"
    );
    assert_eq!(
        run_flow.input_schema()["properties"]["flowLockId"]["type"],
        "string"
    );
    assert_eq!(
        run_flow.input_schema()["properties"]["lockId"]["type"],
        "string"
    );
    assert_eq!(
        run_flow.input_schema()["properties"]["contentHash"]["type"],
        "string"
    );
    assert_eq!(
        run_flow.input_schema()["properties"]["review_id"]["type"],
        "string"
    );
    assert_eq!(
        run_flow.input_schema()["properties"]["reviewId"]["type"],
        "string"
    );
    assert_required_alias_group(run_flow.input_schema(), &["review_id", "reviewId"]);
    assert!(
        run_flow.input_schema()["properties"]
            .get("review_required")
            .is_none()
    );
    assert_eq!(
        run_flow.input_schema()["properties"]["tmux"]["properties"]["enabled"]["type"],
        "boolean"
    );
    assert_eq!(
        run_flow.input_schema()["properties"]["tmux"]["properties"]["agent_command"]["type"],
        "string"
    );
    assert_eq!(
        run_flow.input_schema()["properties"]["tmux"]["properties"]["agentCommand"]["type"],
        "string"
    );
    assert_eq!(
        run_flow.input_schema()["properties"]["tmux"]["properties"]["prompt_submit_key_count"]["type"],
        "integer"
    );
    assert_eq!(
        run_flow.input_schema()["properties"]["tmux"]["properties"]["agent_ready_pattern"]["type"],
        "string"
    );
    assert_eq!(
        run_flow.input_schema()["properties"]["tmux"]["properties"]["agent_ready_timeout_ms"]["type"],
        "integer"
    );

    for name in [
        "run_status",
        "run_why",
        "pause_run",
        "resume_run",
        "stop_run",
    ] {
        let descriptor = surface.lookup(name).expect("descriptor should be present");
        assert_eq!(descriptor.input_schema()["required"], json!([]));
        assert_required_alias_group(descriptor.input_schema(), &["run_id", "runId"]);
        assert_eq!(
            descriptor.input_schema()["properties"]["runId"]["type"],
            "string"
        );
    }

    let prepare = surface
        .lookup("prepare_flow_review")
        .expect("prepare_flow_review descriptor should be present");
    assert_eq!(
        prepare.input_schema()["properties"]["flow_lock_id"]["type"],
        "string"
    );

    let decide = surface
        .lookup("decide_flow_review")
        .expect("decide_flow_review descriptor should be present");
    assert_eq!(
        decide.input_schema()["required"],
        json!(["review_id", "decision"])
    );

    let propose = surface
        .lookup("propose_flow_update")
        .expect("propose_flow_update descriptor should be present");
    assert_eq!(propose.input_schema()["required"], json!(["flow"]));
    assert!(propose.input_schema()["properties"].get("run_id").is_none());
    assert_eq!(
        propose.input_schema()["properties"]["applyMode"]["type"],
        "string"
    );
    assert!(
        propose.input_schema()["properties"]
            .get("reviewRequired")
            .is_none()
    );

    let apply = surface
        .lookup("apply_flow_update")
        .expect("apply_flow_update descriptor should be present");
    assert_eq!(apply.input_schema()["required"], json!([]));
    assert_required_alias_group(apply.input_schema(), &["run_id", "runId"]);
    assert_required_alias_group(
        apply.input_schema(),
        &["flow_lock_id", "flowLockId", "lock_id", "lockId"],
    );
    assert_required_alias_group(apply.input_schema(), &["content_hash", "contentHash"]);
    assert_required_alias_group(apply.input_schema(), &["review_id", "reviewId"]);
    assert_eq!(
        apply.input_schema()["properties"]["applyMode"]["type"],
        "string"
    );

    let apply_lock = surface
        .lookup("apply_flow_lock")
        .expect("apply_flow_lock descriptor should be present");
    assert_eq!(apply_lock.input_schema()["required"], json!(["mode"]));
    assert_required_alias_group(apply_lock.input_schema(), &["run_id", "runId"]);
    assert_required_alias_group(
        apply_lock.input_schema(),
        &["lock_id", "lockId", "flow_lock_id", "flowLockId"],
    );
    assert_required_alias_group(apply_lock.input_schema(), &["content_hash", "contentHash"]);
    assert_required_alias_group(apply_lock.input_schema(), &["review_id", "reviewId"]);
    for name in [
        "runId",
        "lockId",
        "flow_lock_id",
        "flowLockId",
        "contentHash",
        "reviewId",
    ] {
        assert_eq!(
            apply_lock.input_schema()["properties"][name]["type"],
            "string"
        );
    }
}

#[test]
fn resume_run_schema_exposes_ambiguous_delivery_resolution_contract() {
    let descriptor = McpSurface
        .lookup("resume_run")
        .expect("resume_run descriptor should be present");
    assert!(descriptor.description().contains("ambiguous_delivery"));
    assert!(descriptor.description().contains("started_event_sequence"));
    let resolution = &descriptor.input_schema()["properties"]["delivery_resolution"];
    assert_eq!(resolution["type"], "object");
    assert_eq!(
        resolution["required"],
        json!(["started_event_sequence", "outcome", "evidence"])
    );
    assert_eq!(
        resolution["properties"]["started_event_sequence"]["type"],
        "integer"
    );
    assert_eq!(
        resolution["properties"]["started_event_sequence"]["minimum"],
        1
    );
    assert_eq!(
        resolution["properties"]["outcome"]["enum"],
        json!(["submitted", "not_submitted"])
    );
    assert_eq!(resolution["properties"]["evidence"]["minLength"], 1);
    assert_eq!(resolution["properties"]["evidence"]["pattern"], r".*\S.*");
    assert!(resolution["properties"].get("activation_id").is_none());
    assert!(resolution["properties"].get("role").is_none());
}

#[test]
fn runtime_flow_descriptions_surface_run_flow_recovery() {
    let surface = McpSurface;

    for name in ["apply_flow_lock", "preview_flow_routes"] {
        let descriptor = surface.lookup(name).expect("tool should be present");
        assert!(
            descriptor.description().contains("run_flow"),
            "{name} description should mention run_flow"
        );
    }
}

#[test]
fn runtime_tool_names_include_preview_flow_routes() {
    let surface = McpSurface;
    let names: Vec<_> = surface
        .runtime_tools()
        .iter()
        .map(|tool| tool.name())
        .collect();

    assert!(names.contains(&"preview_flow_routes"));
}
#[test]
fn tools_list_includes_preview_flow_routes_descriptor() {
    let mut server = McpServer::new();

    let response = server
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }))
        .expect("tools/list should produce a response");
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools should be an array");

    assert!(
        tools
            .iter()
            .any(|tool| tool["name"] == "preview_flow_routes")
    );
}
#[test]
fn tools_call_rejects_non_object_arguments() {
    let mut server = McpServer::new();

    let response = call_tool(&mut server, 1, "deliver_artifact", json!("not-object"));

    assert_eq!(response["error"]["code"], -32602);
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("arguments")
    );
}
#[test]
fn deliver_artifact_rejects_missing_required_arguments() {
    let mut server = McpServer::new();

    let response = call_tool(&mut server, 1, "deliver_artifact", json!({}));

    assert_eq!(response["error"]["code"], -32602);
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("run_id")
    );
}
#[test]
fn tools_list_notification_has_no_response() {
    let mut server = McpServer::new();

    let response = server.handle_json_rpc(json!({
        "jsonrpc": "2.0",
        "method": "tools/list"
    }));

    assert_eq!(response, None);
}

#[test]
fn initialize_result_includes_server_wide_workflow_instructions() {
    let mut server = McpServer::new();

    let response = server
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize"
        }))
        .expect("initialize should produce a response");
    let instructions = response["result"]["instructions"]
        .as_str()
        .expect("initialize result should include instructions");
    let prefix: String = instructions.chars().take(512).collect();

    for expected in [
        "Humanize",
        "flow_suggest",
        "flow_check",
        "flow_lock",
        "prepare_flow_review",
        "decide_flow_review",
        "run_flow",
        "do not substitute ordinary repo exploration",
        "root README.md before locking or running",
    ] {
        assert!(
            prefix.contains(expected),
            "initialize instructions should mention {expected:?} in the first 512 chars"
        );
    }
}
#[test]
fn cli_list_tools_emits_json_tool_descriptors() {
    let output = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .arg("--list-tools")
        .output()
        .expect("binary should run");

    assert!(output.status.success());
    let payload: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    let names: Vec<_> = payload["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool should have a name"))
        .collect();

    assert_eq!(names, expected_tool_names());
}

#[test]
fn stdio_project_local_home_exits_with_configuration_error_without_panic() {
    let project = child_asset_root("mcp-project-local-home");
    std::fs::create_dir_all(&project).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .current_dir(&project)
        .env("HOME", &project)
        .env("RUST_BACKTRACE", "1")
        .env_remove("HUMANIZE_STATE_ROOT")
        .env_remove("XDG_STATE_HOME")
        .output()
        .expect("binary should run");
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "unexpected stdout: {:?}",
        output.stdout
    );
    assert!(stderr.contains("MCP configuration error"), "{stderr}");
    assert!(!stderr.contains("panicked at"), "{stderr}");
    assert!(!stderr.contains("stack backtrace"), "{stderr}");
    assert!(
        !stderr.contains(project.to_string_lossy().as_ref()),
        "{stderr}"
    );

    std::fs::remove_dir_all(project).unwrap();
}

#[test]
fn stdio_json_rpc_smoke_handles_initialize_list_and_calls() {
    let asset_root = child_asset_root("mcp-surface-stdio-assets");
    let mut child = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .env("HUMANIZE_RUNS_DIR", &asset_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("binary should spawn");

    {
        let stdin = child.stdin.as_mut().expect("stdin should be piped");
        for message in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"flow_check","arguments":{"flow":{"nodes":["root"],"resources":[readme_resource()]},"mode":"core"}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"flow_repair","arguments":{"flow":{"nodes":["root"],"resources":[readme_resource()]}}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"run_flow","arguments":{"run_id":"run-a","nodes":["root"],"review_id":"review-missing"}}}),
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"run_status","arguments":{"run_id":"run-a"}}}),
            json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"run_why","arguments":{"run_id":"run-a"}}}),
        ] {
            writeln!(stdin, "{message}").expect("request should be written");
        }
    }

    let output = child.wait_with_output().expect("child should exit");
    assert!(output.status.success());
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .expect("stdout should be UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("response should be JSON"))
        .collect();

    assert_eq!(responses.len(), 7);
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "humanize-plugin-mcp"
    );
    assert_eq!(
        responses[1]["result"]["tools"]
            .as_array()
            .expect("tools should be an array")
            .len(),
        expected_tool_names().len()
    );
    assert_eq!(responses[2]["result"]["structuredContent"]["ok"], true);
    assert_eq!(responses[3]["result"]["structuredContent"]["ok"], true);
    assert_eq!(
        responses[4]["result"]["structuredContent"]["run_id"],
        "run-a"
    );
    assert_eq!(responses[4]["result"]["structuredContent"]["ok"], false);
    assert_eq!(
        responses[4]["result"]["structuredContent"]["error"],
        "flow_lock_id is required for driver run_flow"
    );
    assert_eq!(responses[5]["result"]["isError"], true);
    assert_eq!(
        responses[5]["result"]["structuredContent"]["error"]["code"],
        "driver_authority_required"
    );
    assert_eq!(responses[6]["result"]["isError"], true);
    assert_eq!(
        responses[6]["result"]["structuredContent"]["error"]["code"],
        "driver_authority_required"
    );
}
