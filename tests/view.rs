use std::collections::BTreeMap;

use humanize_plugin::runtime::{BoardPatch, NodeSpec, Runtime, StopContract};
use humanize_plugin::view::{
    AdapterCapabilityReview, DiffEntry, FlowGraph, FlowGraphEdge, FlowGraphNode,
    FlowReviewContract, FlowReviewNode, FlowReviewRoute, FlowReviewSnapshot, FlowValueFlow,
    FlowVisualDiff, PaneMappingSnapshot, ReviewRisk, RuntimeBudgetSnapshot,
    RuntimeDecisionSnapshot, RuntimeEventSnapshot, VisualizationSnapshot, render_browser_document,
    render_flow_review_document, render_terminal_dashboard, snapshot_json,
};
use serde_json::Value;

fn runtime_with_view_data() -> Runtime {
    let mut runtime = Runtime::default();
    let root = NodeSpec::new("root")
        .with_stop_contract(StopContract::new(["brief", "report"], ["shell", "review"]));

    runtime.start_run("run-a", vec![root]).unwrap();
    runtime
        .deliver_artifact("run-a", "root", "brief", "ready")
        .unwrap();
    runtime
        .patch_board("run-a", "root", BoardPatch::new("summary", "ready"))
        .unwrap();
    runtime
        .record_effect("run-a", "root", "shell", "cargo test")
        .unwrap();

    runtime
}

#[test]
fn visualization_snapshot_projects_runtime_counts_and_stop_contract_gaps() {
    let runtime = runtime_with_view_data();
    let message_counts = BTreeMap::from([("run-a".to_string(), 1)]);

    let snapshot = VisualizationSnapshot::from_runtime(runtime.state(), &message_counts);

    assert_eq!(snapshot.runs.len(), 1);
    let run = &snapshot.runs[0];
    assert_eq!(run.run_id, "run-a");
    assert_eq!(run.activation_count, 1);
    assert_eq!(run.artifact_count, 1);
    assert_eq!(run.effect_count, 1);
    assert_eq!(run.board_version, 1);
    assert_eq!(run.message_count, 1);
    assert_eq!(run.missing_stop_contract_count, 2);
    assert_eq!(
        run.missing_stop_contracts.get("root").cloned(),
        Some(vec![
            "artifact:report".to_string(),
            "effect:review".to_string()
        ])
    );
}

#[test]
fn terminal_dashboard_is_compact_and_deterministic() {
    let runtime = runtime_with_view_data();
    let message_counts = BTreeMap::from([("run-a".to_string(), 1)]);
    let mut snapshot = VisualizationSnapshot::from_runtime(runtime.state(), &message_counts);
    let run = snapshot.run_mut("run-a").unwrap();
    run.why = Some("brief exists and report is still missing".to_string());
    run.last_decision = Some(RuntimeDecisionSnapshot {
        decision_id: "route-root-review".to_string(),
        summary: "hold root until report is ready".to_string(),
        why: "contract gap blocks completion".to_string(),
    });
    run.event_timeline.push(RuntimeEventSnapshot {
        sequence: 7,
        label: "artifact received".to_string(),
        detail: "brief from root".to_string(),
    });
    run.pane_mappings.push(PaneMappingSnapshot {
        activation_id: "root".to_string(),
        pane: "%1".to_string(),
        status: "reserved".to_string(),
    });
    run.runtime_budgets.push(RuntimeBudgetSnapshot {
        name: "review tokens".to_string(),
        used: 320,
        limit: 1000,
        unit: "tokens".to_string(),
    });
    run.activations.get_mut("root").unwrap().pane = Some(PaneMappingSnapshot {
        activation_id: "root".to_string(),
        pane: "%1".to_string(),
        status: "reserved".to_string(),
    });

    let terminal = render_terminal_dashboard(&snapshot);

    assert_eq!(
        terminal,
        concat!(
            "humanize dashboard\n",
            "runs 1\n",
            "run run-a | activations 1 | board version 1 | messages 1 | artifacts 1 | effects 1 | missing 2 | status ready | panes 1\n",
            "  why brief exists and report is still missing\n",
            "  last decision route-root-review | hold root until report is ready | why contract gap blocks completion\n",
            "  event 7 | artifact received | brief from root\n",
            "  pane root | %1 | reserved\n",
            "  budget review tokens | 320/1000 tokens\n",
            "  root | node root | missing artifact:report, effect:review | status pending | pane %1\n"
        )
    );
    assert_eq!(render_terminal_dashboard(&snapshot), terminal);
}

#[test]
fn flow_review_document_renders_static_graph_contracts_capabilities_risks_and_diff() {
    let review = FlowReviewSnapshot {
        title: "Checkout workflow review".to_string(),
        review_status: "needs_review".to_string(),
        graph: FlowGraph {
            nodes: vec![
                FlowGraphNode {
                    id: "collect".to_string(),
                    label: "Collect inputs".to_string(),
                    kind: "agent".to_string(),
                },
                FlowGraphNode {
                    id: "review".to_string(),
                    label: "Review output".to_string(),
                    kind: "review".to_string(),
                },
            ],
            edges: vec![FlowGraphEdge {
                from: "collect".to_string(),
                to: "review".to_string(),
                label: "brief ready".to_string(),
            }],
        },
        nodes: vec![FlowReviewNode {
            id: "collect".to_string(),
            label: "Collect inputs".to_string(),
            contract_id: "contract.collect".to_string(),
            status: "present".to_string(),
            summary: "Produces brief and shell effect evidence".to_string(),
        }],
        routes: vec![FlowReviewRoute {
            id: "collect-to-review".to_string(),
            from: "collect".to_string(),
            to: "review".to_string(),
            predicate: "exists(artifact.brief)".to_string(),
            outcome: "activates review".to_string(),
        }],
        contracts: vec![FlowReviewContract {
            id: "contract.collect".to_string(),
            node_id: "collect".to_string(),
            required_artifacts: vec!["brief".to_string()],
            required_effects: vec!["shell".to_string()],
            summary: "Stop only after brief and shell evidence".to_string(),
        }],
        capabilities: vec![
            AdapterCapabilityReview {
                adapter: "tmux".to_string(),
                capability: "pane_capture".to_string(),
                present: true,
                detail: "available locally".to_string(),
            },
            AdapterCapabilityReview {
                adapter: "browser".to_string(),
                capability: "screenshot".to_string(),
                present: false,
                detail: "not connected in view layer".to_string(),
            },
        ],
        artifact_flows: vec![FlowValueFlow {
            source: "collect".to_string(),
            target: "review".to_string(),
            value: "artifact.brief".to_string(),
        }],
        effect_flows: vec![FlowValueFlow {
            source: "collect".to_string(),
            target: "review".to_string(),
            value: "effect.shell".to_string(),
        }],
        runtime_budgets: vec![RuntimeBudgetSnapshot {
            name: "wall clock".to_string(),
            used: 5,
            limit: 10,
            unit: "minutes".to_string(),
        }],
        risks: vec![ReviewRisk {
            id: "risk.browser".to_string(),
            severity: "medium".to_string(),
            summary: "Screenshot acceptance is deferred".to_string(),
            mitigation: "Keep HTML static and screenshot-free".to_string(),
        }],
        dynamic_diff: FlowVisualDiff {
            added_nodes: vec![DiffEntry::new("review", "review node added")],
            removed_nodes: vec![DiffEntry::new("legacy-check", "old checker removed")],
            changed_nodes: vec![DiffEntry::new("collect", "contract link changed")],
            added_routes: vec![DiffEntry::new("collect-to-review", "brief route added")],
            removed_routes: vec![DiffEntry::new("old-route", "unused route removed")],
            changed_routes: vec![DiffEntry::new("retry-route", "predicate narrowed")],
            added_contracts: vec![DiffEntry::new("contract.collect", "brief required")],
            removed_contracts: vec![DiffEntry::new("contract.legacy", "legacy contract removed")],
            changed_contracts: vec![DiffEntry::new(
                "contract.review",
                "effect requirement changed",
            )],
            capability_changes: vec![DiffEntry::new("browser.screenshot", "missing capability")],
            risk_changes: vec![DiffEntry::new("risk.browser", "acceptance deferred")],
        },
    };

    let html = render_flow_review_document(&review).unwrap();

    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("id=\"flow-review-graph\""));
    assert!(html.contains("Collect inputs"));
    assert!(html.contains("collect -> review"));
    assert!(html.contains("exists(artifact.brief)"));
    assert!(html.contains("contract.collect"));
    assert!(html.contains("pane_capture"));
    assert!(html.contains("present"));
    assert!(html.contains("screenshot"));
    assert!(html.contains("missing"));
    assert!(html.contains("artifact.brief"));
    assert!(html.contains("effect.shell"));
    assert!(html.contains("wall clock"));
    assert!(html.contains("Screenshot acceptance is deferred"));
    assert!(html.contains("Dynamic Update Diff"));
    assert!(html.contains("review node added"));
    assert!(html.contains("old checker removed"));
    assert!(html.contains("predicate narrowed"));
    assert!(html.contains("missing capability"));
    assert!(html.contains("acceptance deferred"));
    assert!(!html.contains("https://"));
    assert!(!html.contains("http://"));
    assert!(html.is_ascii());
}

#[test]
fn browser_document_bootstraps_snapshot_json() {
    let runtime = runtime_with_view_data();
    let message_counts = BTreeMap::from([("run-a".to_string(), 1)]);
    let snapshot = VisualizationSnapshot::from_runtime(runtime.state(), &message_counts);

    let first_json = snapshot_json(&snapshot).unwrap();
    let second_json = snapshot_json(&snapshot).unwrap();
    assert_eq!(first_json, second_json);

    let parsed: Value = serde_json::from_str(&first_json).unwrap();
    assert_eq!(parsed["runs"][0]["run_id"], "run-a");
    assert_eq!(parsed["runs"][0]["artifact_count"], 1);
    assert_eq!(
        parsed["runs"][0]["missing_stop_contracts"]["root"],
        serde_json::json!(["artifact:report", "effect:review"])
    );

    let html = render_browser_document(&snapshot).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("id=\"humanize-view-snapshot\""));
    assert!(html.contains("Humanize Dashboard"));

    let start = html
        .find("<script type=\"application/json\" id=\"humanize-view-snapshot\">")
        .expect("snapshot script should be present")
        + "<script type=\"application/json\" id=\"humanize-view-snapshot\">".len();
    let end = html[start..]
        .find("</script>")
        .expect("snapshot script should close")
        + start;
    let bootstrapped: Value = serde_json::from_str(&html[start..end]).unwrap();
    assert_eq!(bootstrapped, parsed);
}
