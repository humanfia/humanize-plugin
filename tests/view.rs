use std::collections::BTreeMap;

use humanize_plugin::runtime::{BoardPatch, NodeSpec, Runtime, StopContract};
use humanize_plugin::view::{
    VisualizationSnapshot, render_browser_document, render_terminal_dashboard, snapshot_json,
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
    let snapshot = VisualizationSnapshot::from_runtime(runtime.state(), &message_counts);

    let terminal = render_terminal_dashboard(&snapshot);

    assert_eq!(
        terminal,
        concat!(
            "humanize dashboard\n",
            "runs 1\n",
            "run run-a | activations 1 | board v1 | messages 1 | artifacts 1 | effects 1 | missing 2\n",
            "  root | node root | missing artifact:report, effect:review\n"
        )
    );
    assert_eq!(render_terminal_dashboard(&snapshot), terminal);
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
