mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::mcp::McpServer;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::json;

use support::mcp::{RecordingRunner, call_tool, structured, valid_flow};

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
fn prepare_flow_review_returns_document_and_snapshot_sections() {
    let mut server = isolated_server();

    let prepared = call_tool(
        &mut server,
        1,
        "prepare_flow_review",
        json!({ "flow": valid_flow() }),
    );

    assert_eq!(structured(&prepared)["ok"], true);
    assert_eq!(structured(&prepared)["review_status"], "pending");
    assert!(
        structured(&prepared)["review_id"]
            .as_str()
            .is_some_and(|review_id| review_id.starts_with("review_"))
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
    assert_eq!(snapshot["routes"][0]["from"], "fact:artifact.ready");
    assert_eq!(snapshot["routes"][0]["predicate"], "exists(artifact.ready)");
    assert!(
        snapshot["graph"]["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edge| edge["from"] == "fact:artifact.ready" && edge["to"] == "finish")
    );
    assert!(
        structured(&prepared)["document"]
            .as_str()
            .is_some_and(|document| document.contains("Flow Review"))
    );
}

#[test]
fn decide_flow_review_records_bypass_reason_and_rejects_missing_reason() {
    let mut server = isolated_server();
    let prepared = call_tool(
        &mut server,
        1,
        "prepare_flow_review",
        json!({ "flow": valid_flow() }),
    );
    let review_id = structured(&prepared)["review_id"].as_str().unwrap();

    let missing_reason = call_tool(
        &mut server,
        2,
        "decide_flow_review",
        json!({ "review_id": review_id, "decision": "bypassed" }),
    );
    assert_eq!(missing_reason["error"]["code"], -32602);
    assert!(
        missing_reason["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("reason"))
    );

    let bypassed = call_tool(
        &mut server,
        3,
        "decide_flow_review",
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
fn hidden_browser_view_is_rejected_without_local_execution() {
    let mut server = isolated_server();
    let response = call_tool(
        &mut server,
        1,
        "view_browser",
        json!({ "host": "127.0.0.1", "port": 0 }),
    );

    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(response["error"]["message"], "unknown tool");
}
