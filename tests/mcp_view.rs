mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::mcp::McpServer;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use support::mcp::{
    RecordingRunner, call_tool, http_get, populate_view_run, structured, valid_flow,
};

static NEXT_ASSET_ROOT: AtomicU64 = AtomicU64::new(1);

fn isolated_server() -> McpServer<RecordingRunner> {
    let index = NEXT_ASSET_ROOT.fetch_add(1, Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("mcp-view-assets-{index}"));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    McpServer::with_tmux_runner_and_run_asset_store(
        RecordingRunner::default(),
        RunAssetStore::new(RunAssetSink::Root(root)),
    )
}

#[test]
fn view_terminal_returns_dashboard_for_runtime_snapshot() {
    let mut server = isolated_server();
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
        "run run-view | activations 1 | board version 0 | messages 0 | artifacts 1 | effects 1 | missing 2"
    ));
    assert!(dashboard.contains("root | node root | missing artifact:report, effect:review"));
}
#[test]
fn view_snapshot_returns_filterable_structured_snapshot() {
    let mut server = isolated_server();
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
fn prepare_flow_review_returns_document_and_snapshot_sections() {
    let mut server = isolated_server();

    let prepared = call_tool(
        &mut server,
        1,
        "prepare_flow_review",
        json!({
            "flow": valid_flow()
        }),
    );

    assert_eq!(structured(&prepared)["ok"], true);
    assert_eq!(structured(&prepared)["review_status"], "pending");
    assert!(
        structured(&prepared)["review_id"]
            .as_str()
            .expect("review id should be present")
            .starts_with("review_")
    );
    let snapshot = &structured(&prepared)["snapshot"];
    for key in [
        "graph",
        "nodes",
        "routes",
        "contracts",
        "capabilities",
        "risks",
        "dynamic_diff",
    ] {
        assert!(snapshot.get(key).is_some(), "snapshot should include {key}");
    }
    let document = structured(&prepared)["document"]
        .as_str()
        .expect("document should be present");
    assert!(document.contains("Flow Review"));
    assert!(document.contains("Workflow Graph"));
    assert!(document.contains("Dynamic Update Diff"));
}

#[test]
fn approve_flow_review_records_bypass_reason_and_rejects_missing_reason() {
    let mut server = isolated_server();

    let prepared = call_tool(
        &mut server,
        1,
        "prepare_flow_review",
        json!({
            "flow": valid_flow()
        }),
    );
    let review_id = structured(&prepared)["review_id"]
        .as_str()
        .expect("review id should be present");

    let missing_reason = call_tool(
        &mut server,
        2,
        "approve_flow_review",
        json!({
            "review_id": review_id,
            "decision": "bypassed"
        }),
    );
    assert_eq!(missing_reason["error"]["code"], -32602);
    assert!(
        missing_reason["error"]["message"]
            .as_str()
            .expect("error should include a message")
            .contains("reason")
    );

    let bypassed = call_tool(
        &mut server,
        3,
        "approve_flow_review",
        json!({
            "review_id": review_id,
            "decision": "bypassed",
            "reason": "Emergency operator override."
        }),
    );
    assert_eq!(structured(&bypassed)["ok"], true);
    assert_eq!(structured(&bypassed)["review_status"], "bypassed");
    assert_eq!(
        structured(&bypassed)["reason"],
        "Emergency operator override."
    );
}

#[test]
fn view_browser_rejects_non_loopback_host() {
    let mut server = isolated_server();

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
    let mut server = isolated_server();
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
