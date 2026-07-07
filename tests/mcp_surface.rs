use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Write;
use std::process::{Command, Stdio};
use std::rc::Rc;

use humanize_plugin::adapters::tmux::{CommandOutput, CommandRunner, TmuxError};
use humanize_plugin::mcp::{AUTHORING_TOOL_NAMES, McpServer, McpSurface, RUNTIME_TOOL_NAMES};
use serde_json::{Value, json};

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

fn expected_tool_names() -> Vec<&'static str> {
    RUNTIME_TOOL_NAMES
        .iter()
        .chain(AUTHORING_TOOL_NAMES.iter())
        .copied()
        .collect()
}

fn call_tool<R: CommandRunner>(
    server: &mut McpServer<R>,
    id: u64,
    name: &str,
    arguments: Value,
) -> Value {
    server
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }))
        .expect("tool call should produce a response")
}

fn structured(response: &Value) -> &Value {
    &response["result"]["structuredContent"]
}

fn diagnostic_codes(response: &Value) -> Vec<&str> {
    structured(response)["diagnostics"]
        .as_array()
        .expect("diagnostics should be an array")
        .iter()
        .map(|diagnostic| {
            diagnostic["code"]
                .as_str()
                .expect("diagnostic should include a code")
        })
        .collect()
}

fn assert_tool_error(response: &Value) {
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(structured(response)["ok"], false);
}

fn readme_resource() -> Value {
    json!({
        "id": "readme.main",
        "kind": "readme",
        "source": "inline:Use Humanize to audit this library without editing files."
    })
}

fn missing_readme_flow() -> Value {
    json!({
        "nodes": ["root"]
    })
}

fn node_less_missing_readme_flow() -> Value {
    json!({
        "resources": [
            {
                "id": "schema.handoff",
                "kind": "schema",
                "source": "inline:handoff"
            }
        ]
    })
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
fn start_run_schema_requires_tmux_session_and_window_when_enabled() {
    let surface = McpSurface;
    let descriptor = surface
        .lookup("start_run")
        .expect("start_run descriptor should be present");
    let schema = descriptor.input_schema();
    let tmux_schema = &schema["properties"]["tmux"];
    let enabled_case = &tmux_schema["allOf"][0];

    assert_eq!(enabled_case["if"]["required"], json!(["enabled"]));
    assert_eq!(enabled_case["if"]["properties"]["enabled"]["const"], true);
    assert_eq!(
        enabled_case["then"]["required"],
        json!(["session", "window"])
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
            .contains("artifact_key")
    );
}

#[test]
fn flow_check_rejects_effectful_predicate_diagnostics() {
    let mut server = McpServer::new();

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
        let mut server = McpServer::new();

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
    let mut server = McpServer::new();

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
fn flow_check_keeps_core_warning_diagnostics_successful() {
    let mut server = McpServer::new();

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
    let mut server = McpServer::new();

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
    let mut server = McpServer::new();

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
    let mut server = McpServer::new();

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
    let mut server = McpServer::new();

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
    let mut server = McpServer::new();

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
    let mut server = McpServer::new();

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
    let mut server = McpServer::new();

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
fn apply_flow_lock_requires_and_records_lock_provenance() {
    let mut server = McpServer::new();

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-lock",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let missing_provenance = call_tool(
        &mut server,
        2,
        "apply_flow_lock",
        json!({
            "run_id": "run-lock",
            "mode": "future_activations",
            "content_hash": "sha256:first"
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
        3,
        "apply_flow_lock",
        json!({
            "run_id": "run-lock",
            "mode": "future_activations",
            "lock_id": "lock-a",
            "content_hash": "sha256:first"
        }),
    );
    assert_eq!(structured(&applied)["ok"], true);
    assert_eq!(structured(&applied)["lock_id"], "lock-a");
    assert_eq!(structured(&applied)["content_hash"], "sha256:first");

    let context = call_tool(
        &mut server,
        4,
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
    assert_eq!(latest["lock_id"], "lock-a");
    assert_eq!(latest["content_hash"], "sha256:first");
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
fn stdio_json_rpc_smoke_handles_initialize_list_and_calls() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
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
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"start_run","arguments":{"run_id":"run-a","nodes":["root"]}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"deliver_artifact","arguments":{"run_id":"run-a","activation_id":"root","artifact_key":"brief","payload":{"text":"ready"}}}}),
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"validate_stop","arguments":{"run_id":"run-a","activation_id":"root"}}}),
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

    assert_eq!(responses.len(), 6);
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
    assert_eq!(
        responses[3]["result"]["structuredContent"]["run_id"],
        "run-a"
    );
    assert_eq!(
        responses[4]["result"]["structuredContent"]["artifact_key"],
        "brief"
    );
    assert_eq!(responses[5]["result"]["structuredContent"]["valid"], true);
}
