use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::flow::{
    FlowCheckMode, FlowDraft, FlowNode, FlowResource, ResourceKind, flow_lock,
};
use serde_json::{Value, json};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

#[test]
fn review_survives_real_mcp_process_restarts() {
    let root = std::env::temp_dir().join(format!(
        "humanize-mcp-review-restart-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("home")).unwrap();
    let lock = flow_lock(
        &FlowDraft {
            nodes: vec![FlowNode {
                id: "root".into(),
                ..FlowNode::default()
            }],
            resources: vec![FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: "Real MCP restart review fixture.\n".into(),
            }],
            ..FlowDraft::default()
        },
        FlowCheckMode::Core,
    )
    .unwrap();

    let prepared = call_mcp_process(
        &root,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "prepare_flow_review",
                "arguments": {
                    "flow_lock": serde_json::to_value(&lock).unwrap(),
                    "title": "Restart review"
                }
            }
        }),
    );
    let review_id = prepared["result"]["structuredContent"]["review_id"]
        .as_str()
        .unwrap()
        .to_string();

    let approved = call_mcp_process(&root, decision_request(2, &review_id, "approved"));
    assert_eq!(
        approved["result"]["structuredContent"]["review_status"],
        "approved"
    );

    let reloaded = call_mcp_process(&root, decision_request(3, &review_id, "approved"));
    assert_eq!(
        reloaded["result"]["structuredContent"]["flow_lock_id"],
        lock.id()
    );
    assert_eq!(
        reloaded["result"]["structuredContent"]["content_hash"],
        lock.content_hash()
    );
    fs::remove_dir_all(root).unwrap();
}

fn decision_request(id: u64, review_id: &str, decision: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "decide_flow_review",
            "arguments": {
                "review_id": review_id,
                "decision": decision
            }
        }
    })
}

fn call_mcp_process(root: &Path, request: Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .env("HOME", root.join("home"))
        .env("HUMANIZE_STATE_ROOT", root.join("state"))
        .env("HUMANIZE_RUNS_DIR", root.join("runs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "MCP process failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()
}
