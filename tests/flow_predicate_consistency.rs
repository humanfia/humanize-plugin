mod support;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::flow::{
    FactRef, FlowCheckMode, FlowDraft, FlowNode, FlowPredicate, FlowResource, FlowRoute,
    ResourceKind, flow_check, flow_lock,
};
use humanize_plugin::mcp::McpServer;
use humanize_plugin::review::ReviewStore;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use humanize_plugin::runtime::{BoardPatch, NodeSpec, Runtime, preview_flow_routes};
use humanize_plugin::view::derive_flow_graph;
use serde_json::{Value, json};

use support::mcp::{RecordingRunner, call_tool, structured};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

#[test]
fn predicate_semantics_match_serde_check_review_preview_runtime_and_graph() {
    let cases = vec![
        (
            FlowPredicate::exists_artifact("value").unwrap(),
            None,
            false,
        ),
        (
            FlowPredicate::exists_artifact("value").unwrap(),
            Some(""),
            true,
        ),
        (
            FlowPredicate::exists_artifact("value").unwrap(),
            Some("false"),
            true,
        ),
        (
            FlowPredicate::exists_artifact("value").unwrap(),
            Some("0"),
            true,
        ),
        (
            FlowPredicate::truthy_artifact("value").unwrap(),
            Some(""),
            false,
        ),
        (
            FlowPredicate::truthy_artifact("value").unwrap(),
            Some("false"),
            false,
        ),
        (
            FlowPredicate::truthy_artifact("value").unwrap(),
            Some("0"),
            false,
        ),
        (
            FlowPredicate::truthy_artifact("value").unwrap(),
            Some("yes"),
            true,
        ),
        (FlowPredicate::exists_board("value").unwrap(), None, false),
        (
            FlowPredicate::exists_board("value").unwrap(),
            Some(""),
            true,
        ),
        (
            FlowPredicate::exists_board("value").unwrap(),
            Some("false"),
            true,
        ),
        (
            FlowPredicate::exists_board("value").unwrap(),
            Some("0"),
            true,
        ),
        (
            FlowPredicate::truthy_board("value").unwrap(),
            Some(""),
            false,
        ),
        (
            FlowPredicate::truthy_board("value").unwrap(),
            Some("false"),
            false,
        ),
        (
            FlowPredicate::truthy_board("value").unwrap(),
            Some("0"),
            false,
        ),
        (
            FlowPredicate::truthy_board("value").unwrap(),
            Some("1"),
            true,
        ),
    ];

    for (index, (predicate, value, expected)) in cases.into_iter().enumerate() {
        let draft = draft_with_predicate(predicate.clone());
        let wire = serde_json::to_value(&predicate).unwrap();
        assert_eq!(
            serde_json::from_value::<FlowPredicate>(wire.clone()).unwrap(),
            predicate
        );
        assert!(
            flow_check(&draft, FlowCheckMode::Core)
                .diagnostics
                .is_empty()
        );
        let lock = flow_lock(&draft, FlowCheckMode::Core).unwrap();
        assert_eq!(lock.draft().routes[0].predicate, predicate);

        let root = test_root("predicate-consistency");
        let review_root = root.join("reviews");
        let mut server = McpServer::with_tmux_runner_and_run_asset_store(
            RecordingRunner::default(),
            RunAssetStore::new(RunAssetSink::Root(root.clone())),
        )
        .with_review_store(ReviewStore::new(review_root));
        let locked = call_tool(
            &mut server,
            index as u64 + 1,
            "flow_lock",
            json!({ "flow": draft_json(&predicate) }),
        );
        assert_eq!(structured(&locked)["ok"], true, "{locked}");
        let reviewed = call_tool(
            &mut server,
            index as u64 + 100,
            "prepare_flow_review",
            json!({
                "flow_lock_id": structured(&locked)["flow_lock_id"],
                "content_hash": structured(&locked)["content_hash"]
            }),
        );
        assert_eq!(structured(&reviewed)["ok"], true, "{reviewed}");
        assert_eq!(
            structured(&reviewed)["snapshot"]["routes"][0]["predicate"],
            predicate.to_string()
        );

        let mut runtime = Runtime::default();
        runtime
            .start_run("run-predicate", vec![NodeSpec::new("root")])
            .unwrap();
        if let Some(value) = value {
            match predicate.fact_ref() {
                FactRef::Artifact { key } => {
                    runtime
                        .deliver_artifact("run-predicate", "root", key.as_str(), value)
                        .unwrap();
                }
                FactRef::Board { key } => {
                    runtime
                        .patch_board(
                            "run-predicate",
                            "root",
                            BoardPatch::new(key.as_str(), value).unwrap(),
                        )
                        .unwrap();
                }
            }
        }
        let preview = preview_flow_routes(runtime.state(), "run-predicate", &lock).unwrap();
        assert_eq!(preview[0].matched, expected, "predicate {predicate}");

        let graph = derive_flow_graph(lock.draft());
        assert!(graph.edges.iter().any(|edge| {
            edge.from == format!("fact:{}", predicate.fact_ref()) && edge.to == "target"
        }));
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn invalid_fact_references_are_rejected_at_typed_serde_and_mcp_boundaries() {
    for (index, key) in ["", ".value", "value.", "bad-key", "bad..key", "caf\u{e9}"]
        .into_iter()
        .enumerate()
    {
        let predicate = json!({
            "op": "exists",
            "fact": {"kind": "artifact", "key": key}
        });
        assert!(serde_json::from_value::<FlowPredicate>(predicate.clone()).is_err());

        let root = test_root("predicate-invalid");
        let mut server = McpServer::with_tmux_runner_and_run_asset_store(
            RecordingRunner::default(),
            RunAssetStore::new(RunAssetSink::Root(root.clone())),
        )
        .with_review_store(ReviewStore::new(root.join("reviews")));
        let checked = call_tool(
            &mut server,
            index as u64 + 200,
            "flow_check",
            json!({ "flow": draft_json_value(predicate) }),
        );
        assert_eq!(checked["error"]["code"], -32602, "{key:?}: {checked}");
        if root.exists() {
            fs::remove_dir_all(root).unwrap();
        }
    }
}

fn draft_with_predicate(predicate: FlowPredicate) -> FlowDraft {
    FlowDraft {
        nodes: vec![
            FlowNode {
                id: "root".into(),
                ..FlowNode::default()
            },
            FlowNode {
                id: "target".into(),
                ..FlowNode::default()
            },
        ],
        routes: vec![FlowRoute {
            predicate,
            for_each: None,
            activate: "target".into(),
        }],
        resources: vec![FlowResource {
            id: "README.md".into(),
            kind: ResourceKind::Readme,
            source: "Predicate consistency fixture.\n".into(),
        }],
        ..FlowDraft::default()
    }
}

fn draft_json(predicate: &FlowPredicate) -> Value {
    draft_json_value(serde_json::to_value(predicate).unwrap())
}

fn draft_json_value(predicate: Value) -> Value {
    json!({
        "nodes": ["root", "target"],
        "routes": [{
            "predicate": predicate,
            "activate": "target"
        }],
        "resources": [{
            "path": "README.md",
            "kind": "readme",
            "content": "Predicate consistency fixture.\n"
        }]
    })
}

fn test_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::SeqCst)
        ))
}
