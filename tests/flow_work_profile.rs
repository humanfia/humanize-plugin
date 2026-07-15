mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::flow::{
    FlowLock, FlowQosIntent, NetworkAccess, QosUrgency, ToolExecution, WorkIntent, WorkProfile,
    WorkspaceAccess, flow_draft_qos, flow_node_work_profile,
};
use humanize_plugin::mcp::McpServer;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use support::mcp::{RecordingRunner, call_tool, readme_resource, structured};

static NEXT_ASSET_ROOT: AtomicU64 = AtomicU64::new(1);

fn isolated_server() -> McpServer<RecordingRunner> {
    let index = NEXT_ASSET_ROOT.fetch_add(1, Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("flow-work-profile-assets-{index}"));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    McpServer::with_tmux_runner_and_run_asset_store(
        RecordingRunner::default(),
        RunAssetStore::new(RunAssetSink::Root(root)),
    )
}

#[test]
fn flow_lock_exports_work_profile_and_qos_only_when_authored() {
    let mut server = isolated_server();

    let locked = call_tool(
        &mut server,
        1,
        "flow_lock",
        json!({
            "mode": "core",
            "flow": {
                "qos": {
                    "urgency": "interactive",
                    "completion_target": "artifact.final_summary"
                },
                "nodes": [
                    {
                        "id": "investigate",
                        "work_profile": {
                            "intent": "explore",
                            "workspace_access": "read_only",
                            "tool_execution": "allowed",
                            "network_access": "restricted"
                        },
                        "action": {
                            "driver": "agent",
                            "reads": ["artifact.issue"],
                            "writes": ["artifact.findings"]
                        }
                    },
                    {
                        "id": "review",
                        "workProfile": {
                            "intent": "evaluate",
                            "workspaceAccess": "none",
                            "toolExecution": "none",
                            "networkAccess": "none"
                        }
                    }
                ],
                "resources": [readme_resource()]
            }
        }),
    );

    assert_eq!(locked["result"]["isError"], false);
    let lock_id = structured(&locked)["flow_lock_id"].as_str().unwrap();
    let exported = call_tool(
        &mut server,
        2,
        "flow_export",
        json!({
            "flow_lock_id": lock_id,
            "format": "json"
        }),
    );

    let document = structured(&exported)["document"].as_str().unwrap();
    let exported_json: Value = serde_json::from_str(document).unwrap();
    assert!(exported_json["flow"].is_object());
    assert!(exported_json.get("content").is_none());

    let exported_lock = serde_json::from_str::<FlowLock>(document).unwrap();
    assert_eq!(
        flow_draft_qos(exported_lock.draft()),
        FlowQosIntent {
            urgency: QosUrgency::Interactive,
            completion_target: Some("artifact.final_summary".into()),
        }
    );
    let investigate = exported_lock
        .draft()
        .nodes
        .iter()
        .find(|node| node.id == "investigate")
        .unwrap();
    assert_eq!(
        flow_node_work_profile(investigate),
        WorkProfile {
            intent: WorkIntent::Explore,
            workspace_access: WorkspaceAccess::ReadOnly,
            tool_execution: ToolExecution::Allowed,
            network_access: NetworkAccess::Restricted,
        }
    );
    let review = exported_lock
        .draft()
        .nodes
        .iter()
        .find(|node| node.id == "review")
        .unwrap();
    assert_eq!(
        flow_node_work_profile(review),
        WorkProfile {
            intent: WorkIntent::Evaluate,
            workspace_access: WorkspaceAccess::None,
            tool_execution: ToolExecution::None,
            network_access: NetworkAccess::None,
        }
    );
}

#[test]
fn flow_check_rejects_empty_qos_completion_target() {
    let mut server = isolated_server();

    let response = call_tool(
        &mut server,
        1,
        "flow_check",
        json!({
            "mode": "core",
            "flow": {
                "qos": {
                    "urgency": "background",
                    "completion_target": ""
                },
                "nodes": ["root"],
                "resources": [readme_resource()]
            }
        }),
    );

    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        structured(&response)["diagnostics"][0]["code"],
        "FLOW_EMPTY_QOS_COMPLETION_TARGET"
    );
    assert_eq!(
        structured(&response)["diagnostics"][0]["location"],
        "qos.completion_target"
    );
}

#[test]
fn flow_check_rejects_unknown_work_profile_values_at_parse_time() {
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
                        "work_profile": {
                            "intent": "benchmark"
                        }
                    }
                ],
                "resources": [readme_resource()]
            }
        }),
    );

    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown work intent")
    );
}
