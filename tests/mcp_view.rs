mod support;

use humanize_plugin::mcp::McpServer;
use serde_json::{Value, json};

use support::mcp::{call_tool, http_get, populate_view_run, structured};

#[test]
fn view_terminal_returns_dashboard_for_runtime_snapshot() {
    let mut server = McpServer::new();
    populate_view_run(&mut server, "run-view");

    let viewed = call_tool(&mut server, 4, "view_terminal", json!({}));

    assert_eq!(structured(&viewed)["ok"], true);
    assert_eq!(structured(&viewed)["format"], "terminal");
    assert_eq!(structured(&viewed)["run_count"], 1);
    let dashboard = structured(&viewed)["dashboard"]
        .as_str()
        .expect("dashboard should be text");
    assert!(dashboard.contains("humanize dashboard"));
    assert!(dashboard.contains(
        "run run-view | activations 1 | board v0 | messages 0 | artifacts 1 | effects 1 | missing 2"
    ));
    assert!(dashboard.contains("root | node root | missing artifact:report, effect:review"));
}
#[test]
fn view_snapshot_returns_filterable_structured_snapshot() {
    let mut server = McpServer::new();
    populate_view_run(&mut server, "run-view-a");
    populate_view_run(&mut server, "run-view-b");

    let viewed = call_tool(
        &mut server,
        4,
        "view_snapshot",
        json!({
            "run_id": "run-view-b"
        }),
    );

    assert_eq!(structured(&viewed)["ok"], true);
    assert_eq!(structured(&viewed)["format"], "json");
    assert_eq!(structured(&viewed)["run_count"], 1);
    assert_eq!(
        structured(&viewed)["snapshot"]["runs"][0]["run_id"],
        "run-view-b"
    );
    assert_eq!(
        structured(&viewed)["snapshot"]["runs"][0]["missing_stop_contracts"]["root"],
        json!(["artifact:report", "effect:review"])
    );

    let missing = call_tool(
        &mut server,
        5,
        "view_snapshot",
        json!({
            "run_id": "missing-run"
        }),
    );
    assert_eq!(missing["error"]["code"], -32602);
    assert!(
        missing["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("missing-run")
    );
}
#[test]
fn view_browser_rejects_non_loopback_host() {
    let mut server = McpServer::new();

    let viewed = call_tool(
        &mut server,
        1,
        "view_browser",
        json!({
            "host": "0.0.0.0",
            "port": 0
        }),
    );

    assert_eq!(viewed["error"]["code"], -32602);
    assert!(
        viewed["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("loopback")
    );
}
#[test]
fn view_browser_serves_html_and_snapshot_json_from_local_port() {
    let mut server = McpServer::new();
    populate_view_run(&mut server, "run-browser");

    let viewed = call_tool(
        &mut server,
        4,
        "view_browser",
        json!({
            "host": "127.0.0.1",
            "port": 0
        }),
    );

    assert_eq!(structured(&viewed)["ok"], true);
    assert_eq!(structured(&viewed)["host"], "127.0.0.1");
    assert_eq!(structured(&viewed)["run_count"], 1);
    let port = structured(&viewed)["port"]
        .as_u64()
        .expect("port should be numeric");
    assert_ne!(port, 0);
    assert_eq!(
        structured(&viewed)["url"],
        format!("http://127.0.0.1:{port}/")
    );

    let html_response = http_get("127.0.0.1", port, "/");
    assert!(html_response.starts_with("HTTP/1.1 200 OK"));
    assert!(html_response.contains("Content-Type: text/html; charset=utf-8"));
    assert!(html_response.contains("<title>Humanize Dashboard</title>"));
    assert!(html_response.contains("run-browser"));

    let json_response = http_get("127.0.0.1", port, "/snapshot.json");
    assert!(json_response.starts_with("HTTP/1.1 200 OK"));
    assert!(json_response.contains("Content-Type: application/json"));
    let body = json_response
        .split("\r\n\r\n")
        .nth(1)
        .expect("HTTP response should include a body");
    let snapshot: Value = serde_json::from_str(body).expect("snapshot should be JSON");
    assert_eq!(snapshot["runs"][0]["run_id"], "run-browser");

    let missing_response = http_get("127.0.0.1", port, "/missing");
    assert!(missing_response.starts_with("HTTP/1.1 404 Not Found"));
}
