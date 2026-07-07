mod support;

use std::io::Write;
use std::process::{Command, Stdio};

use humanize_plugin::mcp::{McpServer, McpSurface};
use serde_json::{Value, json};

use support::mcp::{call_tool, readme_resource};

fn expected_tool_names() -> Vec<&'static str> {
    vec![
        "start_run",
        "get_context",
        "deliver_artifact",
        "fanout_from_artifact",
        "record_effect",
        "patch_board",
        "activate_node",
        "send_message",
        "validate_stop",
        "apply_flow_lock",
        "view_terminal",
        "view_snapshot",
        "view_browser",
        "flow_apply",
        "flow_suggest",
        "flow_check",
        "flow_lock",
        "flow_export",
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
fn fanout_from_artifact_schema_requires_runtime_arguments() {
    let surface = McpSurface;
    let descriptor = surface
        .lookup("fanout_from_artifact")
        .expect("fanout_from_artifact descriptor should be present");
    let schema = descriptor.input_schema();

    assert_eq!(
        schema["required"],
        json!(["run_id", "node_id", "artifact_key"])
    );
    assert_eq!(schema["properties"]["run_id"]["type"], "string");
    assert_eq!(schema["properties"]["node_id"]["type"], "string");
    assert_eq!(schema["properties"]["artifact_key"]["type"], "string");
    assert_eq!(schema["properties"]["for_each"]["type"], "string");
    assert_eq!(schema["properties"]["forEach"]["type"], "string");
    assert_eq!(schema["properties"]["required_artifacts"]["type"], "array");
    assert_eq!(schema["properties"]["required_effects"]["type"], "array");
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
