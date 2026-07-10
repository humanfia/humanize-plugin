mod support;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use humanize_plugin::adapters::tmux::{CommandOutput, CommandRunner, TmuxError};
use humanize_plugin::mcp::McpServer;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use support::mcp::{
    RecordingRunner, acknowledge_pipe_command, call_tool, complete_pipe_command, lock_flow,
    structured,
};

fn test_temp_dir(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(name);
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    path
}

#[test]
fn start_run_creates_pending_manifest_and_captures_non_agent_tmux_panes() {
    let asset_root = test_temp_dir("mcp-run-assets-start-run");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-start-assets",
            "nodes": [
                { "id": "root" },
                { "id": "reviewer" }
            ],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );

    assert_eq!(structured(&started)["ok"], true);
    let run_assets = &structured(&started)["run_assets"];
    assert_eq!(run_assets["flow"]["status"], "pending");
    assert_eq!(run_assets["flow"]["complete"], false);
    assert_eq!(run_assets["flow"]["current_export_path"], Value::Null);
    assert_eq!(
        run_assets["manifest_path"].as_str().unwrap(),
        run_root(&asset_root, "run-start-assets")
            .join("manifest.json")
            .to_string_lossy()
            .as_ref()
    );
    assert!(
        !run_root(&asset_root, "run-start-assets")
            .join("flow/current/flow-lock.json")
            .exists()
    );

    let pipe_calls = tmux_calls(&runner, "pipe-pane");
    assert_eq!(pipe_calls.len(), 2);
    assert_eq!(pipe_calls[0][4], "host-a:%7.%8");
    assert_eq!(pipe_calls[1][4], "host-a:%7.%9");
    assert!(pipe_calls[0][5].contains("transcript.pipe.log"));
    assert!(pipe_calls[1][5].contains("transcript.pipe.log"));

    let manifest_json = read_manifest(&asset_root, "run-start-assets");
    assert_eq!(
        manifest_json["activations"]["root"]["relative_paths"]["metadata"],
        read_manifest(&asset_root, "run-start-assets")["activations"]["root"]["relative_paths"]["metadata"]
    );
    assert_eq!(
        manifest_json["activations"]["reviewer"]["relative_paths"]["transcript_pipe"],
        read_manifest(&asset_root, "run-start-assets")["activations"]["reviewer"]["relative_paths"]
            ["transcript_pipe"]
    );
    assert_eq!(
        read_json(
            run_root(&asset_root, "run-start-assets").join(
                manifest_json["activations"]["root"]["relative_paths"]["metadata"]
                    .as_str()
                    .unwrap()
            )
        )["run_id"],
        "run-start-assets"
    );
    assert_eq!(
        read_json(
            run_root(&asset_root, "run-start-assets").join(
                manifest_json["activations"]["reviewer"]["relative_paths"]["metadata"]
                    .as_str()
                    .unwrap()
            )
        )["activation_id"],
        "reviewer"
    );
    assert!(
        !PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".humanize")
            .exists()
    );
}

#[test]
fn apply_flow_lock_persists_current_flow_and_marks_manifest_complete() {
    let asset_root = test_temp_dir("mcp-run-assets-apply-lock");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-apply-lock",
            "nodes": [{ "id": "root" }],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(
        structured(&started)["run_assets"]["flow"]["complete"],
        false
    );

    let applied = call_tool(
        &mut server,
        3,
        "apply_flow_lock",
        json!({
            "run_id": "run-apply-lock",
            "mode": "future_activations",
            "lock_id": lock_id,
            "content_hash": content_hash
        }),
    );

    assert_eq!(structured(&applied)["ok"], true);
    let run_assets = &structured(&applied)["run_assets"];
    assert_eq!(run_assets["flow"]["status"], "complete");
    assert_eq!(run_assets["flow"]["complete"], true);
    assert_eq!(run_assets["flow"]["current_revision_id"], "rev-0001");
    assert_eq!(
        run_assets["flow"]["current_export_relative_path"],
        "flow/revisions/rev-0001/flow-lock.json"
    );
    assert_eq!(run_assets["flow"]["revisions"][0]["apply_state"], "applied");
    assert!(
        fs::read_to_string(
            run_root(&asset_root, "run-apply-lock").join("flow/revisions/rev-0001/flow-lock.json")
        )
        .unwrap()
        .contains("readme.main")
    );
    assert_eq!(
        read_manifest(&asset_root, "run-apply-lock")["flow"]["complete"],
        true
    );
}

#[test]
fn run_flow_persists_flow_package_and_starts_transcript_pipe_before_agent_input() {
    let asset_root = test_temp_dir("mcp-run-assets-pipe-before-input");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\tflow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
        CommandOutput::success("host-a\t%7\tflow-a\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-assets",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a",
                "agent_command": "humanize-test-agent"
            }
        }),
    );

    assert_eq!(structured(&started)["ok"], true);
    let run_assets = &structured(&started)["run_assets"];
    assert_eq!(run_assets["flow"]["status"], "complete");
    assert_eq!(
        run_assets["flow"]["current_export_relative_path"],
        "flow/revisions/rev-0001/flow-lock.json"
    );
    assert_eq!(run_assets["flow"]["revisions"][0]["apply_state"], "applied");

    let calls = runner.calls();
    let pipe_index = first_tmux_call_index(&calls, "pipe-pane");
    let first_input_index = first_tmux_call_index(&calls, "display-message");
    assert!(pipe_index < first_input_index);
    assert_eq!(
        &calls[pipe_index][..5],
        ["tmux", "pipe-pane", "-o", "-t", "host-a:%7.%8"]
    );
    assert!(calls[pipe_index][5].contains("--pipe-sink"));
    assert!(calls[pipe_index][5].contains("--root"));
    assert!(calls[pipe_index][5].contains("--relative"));
    assert!(calls[pipe_index][5].contains("transcript.pipe.log"));
}

#[test]
fn nodes_only_run_flow_persists_canonical_flow_package() {
    let asset_root = test_temp_dir("mcp-run-assets-node-only-run-flow");
    let runner = RecordingRunner::default();
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);

    let started = call_tool(
        &mut server,
        1,
        "run_flow",
        json!({
            "run_id": "run-node-only-assets",
            "nodes": ["root"]
        }),
    );

    assert_eq!(structured(&started)["ok"], true);
    assert_eq!(structured(&started)["run_assets"]["flow"]["complete"], true);
    assert_eq!(
        structured(&started)["run_assets"]["flow"]["current_export_relative_path"],
        "flow/revisions/rev-0001/flow-lock.json"
    );
    let exported = fs::read_to_string(
        run_root(&asset_root, "run-node-only-assets")
            .join("flow/revisions/rev-0001/flow-lock.json"),
    )
    .unwrap();
    let exported_json: Value = serde_json::from_str(&exported).unwrap();
    let content = exported_json["content"].as_str().unwrap();
    assert!(content.contains("\"root\""));
    assert!(content.contains("\"readme.main\""));
}

#[test]
fn nodes_only_run_flow_preserves_required_effects_in_runtime_and_export() {
    let asset_root = test_temp_dir("mcp-run-assets-node-only-effects");
    let runner = RecordingRunner::default();
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);

    let started = call_tool(
        &mut server,
        1,
        "run_flow",
        json!({
            "run_id": "run-node-effects",
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
    let status = call_tool(
        &mut server,
        2,
        "run_status",
        json!({
            "run_id": "run-node-effects"
        }),
    );
    assert_eq!(
        structured(&status)["context"]["missing_stop_contracts"]["root"],
        json!(["artifact:brief", "effect:shell"])
    );

    let manifest_json = read_manifest(&asset_root, "run-node-effects");
    let export_relative_path = manifest_json["flow"]["current_export_relative_path"]
        .as_str()
        .unwrap();
    assert_eq!(
        export_relative_path,
        "flow/revisions/rev-0001/flow-lock.json"
    );
    let exported =
        fs::read_to_string(run_root(&asset_root, "run-node-effects").join(export_relative_path))
            .unwrap();
    let exported_json: Value = serde_json::from_str(&exported).unwrap();
    let content = exported_json["content"].as_str().unwrap();
    assert!(content.contains("\"effect_requirements\":[{\"id\":\"shell\",\"required\":true}]"));
}

#[test]
fn flow_package_manifest_failure_blocks_runtime_apply_and_later_resume() {
    let asset_root = test_temp_dir("mcp-run-assets-flow-manifest-failure");
    let runner = RecordingRunner::default();
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-flow-write-fails",
            "nodes": ["root"]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let manifest_path = find_manifest_path(&asset_root, "run-flow-write-fails");
    fs::remove_file(&manifest_path).unwrap();
    fs::create_dir(&manifest_path).unwrap();

    let applied = call_tool(
        &mut server,
        3,
        "apply_flow_lock",
        json!({
            "run_id": "run-flow-write-fails",
            "mode": "future_activations",
            "lock_id": lock_id,
            "content_hash": content_hash
        }),
    );

    assert_eq!(applied["result"]["isError"], true);
    assert_eq!(structured(&applied)["ok"], false);
    assert_eq!(
        structured(&applied)["asset_preservation"]["stage"],
        "flow_package"
    );

    let status = call_tool(
        &mut server,
        4,
        "run_status",
        json!({
            "run_id": "run-flow-write-fails"
        }),
    );
    assert_eq!(structured(&status)["context"]["run_status"], "failed");
    assert!(structured(&status)["context"]["flow_lock_id"].is_null());
    assert_eq!(
        structured(&status)["context"]["run_assets"]["flow"]["complete"],
        false
    );
    assert_eq!(
        structured(&status)["context"]["run_assets"]["preservation_errors"][0]["stage"],
        "flow_package"
    );

    let resumed = call_tool(
        &mut server,
        5,
        "resume_run",
        json!({
            "run_id": "run-flow-write-fails"
        }),
    );
    assert_eq!(resumed["result"]["isError"], true);
    assert_eq!(structured(&resumed)["run_status"], "failed");
}

#[test]
fn pipe_start_failure_reinitializes_capture_for_replacement_pane_before_return() {
    let asset_root = test_temp_dir("mcp-run-assets-pipe-retry");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::failure("pipe failed"),
        CommandOutput::success("failed pane final\n"),
        CommandOutput::success(""),
        CommandOutput::success("%10\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, routed_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-pipe-retry",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-pipe-retry",
            "activation_id": "root",
            "artifact_key": "ready",
            "payload": "true"
        }),
    );

    let resumed = call_tool(
        &mut server,
        4,
        "resume_run",
        json!({
            "run_id": "run-pipe-retry"
        }),
    );

    assert_eq!(resumed["result"]["isError"], true);
    assert_eq!(structured(&resumed)["run_status"], "failed");
    let pipe_calls = tmux_calls(&runner, "pipe-pane");
    assert_eq!(pipe_calls.len(), 3);
    assert_eq!(pipe_calls[1][4], "host-a:%7.%9");
    assert_eq!(pipe_calls[2][4], "host-a:%7.%10");
    assert_eq!(tmux_calls(&runner, "kill-pane")[0][3], "host-a:%7.%9");
    let manifest_json = read_manifest(&asset_root, "run-pipe-retry");
    assert_eq!(manifest_json["activations"]["finish"]["pane_id"], "%10");
    assert_eq!(
        manifest_json["activations"]["finish"]["preservation_status"],
        "capturing"
    );
    assert_eq!(
        manifest_json["preservation_errors"][0]["stage"],
        "pipe_start"
    );
}

#[test]
fn pipe_retry_aborts_when_failed_pane_cleanup_cannot_release_old_handle() {
    let asset_root = test_temp_dir("mcp-run-assets-pipe-retry-kill-fails");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::failure("pipe failed"),
        CommandOutput::success("failed pane final\n"),
        CommandOutput::failure("kill failed"),
        CommandOutput::success("%10\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, routed_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-pipe-retry-kill-fails",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-pipe-retry-kill-fails",
            "activation_id": "root",
            "artifact_key": "ready",
            "payload": "true"
        }),
    );

    let resumed = call_tool(
        &mut server,
        4,
        "resume_run",
        json!({
            "run_id": "run-pipe-retry-kill-fails"
        }),
    );

    assert!(resumed.get("error").is_some() || resumed["result"]["isError"] == true);
    let split_calls = tmux_calls(&runner, "split-window");
    assert_eq!(split_calls.len(), 1);
    assert_eq!(split_calls[0][6], "host-a:%7");
    assert_eq!(tmux_calls(&runner, "kill-pane")[0][3], "host-a:%7.%9");
    let manifest_json = read_manifest(&asset_root, "run-pipe-retry-kill-fails");
    assert_eq!(manifest_json["activations"]["finish"]["pane_id"], "%9");
    assert_eq!(
        manifest_json["activations"]["finish"]["preservation_status"],
        "failed"
    );
}

#[test]
fn apply_flow_update_persists_revision_and_captures_new_tmux_activation() {
    let asset_root = test_temp_dir("mcp-run-assets-apply-update");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
        CommandOutput::success("root final\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-update-assets",
            "nodes": [{ "id": "root" }],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let proposed = call_tool(
        &mut server,
        2,
        "propose_flow_update",
        json!({
            "flow": routed_flow(),
            "apply_mode": "future_activations",
            "review_required": false,
            "summary": "Switch to routed flow."
        }),
    );
    assert_eq!(structured(&proposed)["ok"], true);
    let lock_id = structured(&proposed)["flow_lock_id"].as_str().unwrap();
    let content_hash = structured(&proposed)["content_hash"].as_str().unwrap();

    let applied = call_tool(
        &mut server,
        3,
        "apply_flow_update",
        json!({
            "run_id": "run-update-assets",
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&applied)["ok"], true);
    assert_eq!(
        structured(&applied)["run_assets"]["flow"]["revisions"][0]["relative_path"],
        "flow/revisions/rev-0001/flow-lock.json"
    );

    let resumed = call_tool(
        &mut server,
        4,
        "resume_run",
        json!({
            "run_id": "run-update-assets"
        }),
    );
    assert_eq!(structured(&resumed)["ok"], true);

    let delivered = call_tool(
        &mut server,
        5,
        "deliver_artifact",
        json!({
            "run_id": "run-update-assets",
            "activation_id": "root",
            "artifact_key": "ready",
            "payload": "true"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);

    let observed = call_tool(
        &mut server,
        6,
        "observe_stop",
        json!({
            "run_id": "run-update-assets",
            "activation_id": "root",
            "reason": "route source finished"
        }),
    );
    assert_eq!(structured(&observed)["ok"], true);

    let pipe_calls = tmux_calls(&runner, "pipe-pane");
    assert_eq!(pipe_calls.len(), 2);
    assert!(pipe_calls[1][5].contains("transcript.pipe.log"));

    let manifest_json = read_manifest(&asset_root, "run-update-assets");
    assert_eq!(
        manifest_json["flow"]["revisions"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        manifest_json["activations"]["finish"]["relative_paths"]["metadata"],
        read_json(
            run_root(&asset_root, "run-update-assets").join(
                manifest_json["activations"]["finish"]["relative_paths"]["metadata"]
                    .as_str()
                    .unwrap()
            )
        )["relative_paths"]["metadata"]
    );
    assert_eq!(
        read_json(
            run_root(&asset_root, "run-update-assets").join(
                manifest_json["activations"]["finish"]["relative_paths"]["metadata"]
                    .as_str()
                    .unwrap()
            )
        )["activation_id"],
        "finish"
    );
}

#[test]
fn activate_node_allocates_and_captures_locked_agent_activation_before_interaction() {
    let asset_root = test_temp_dir("mcp-run-assets-activate-node-capture");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, dynamic_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-activate-capture",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let applied = call_tool(
        &mut server,
        3,
        "apply_flow_lock",
        json!({
            "run_id": "run-activate-capture",
            "mode": "future_activations",
            "lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&applied)["ok"], true);

    let activated = call_tool(
        &mut server,
        4,
        "activate_node",
        json!({
            "run_id": "run-activate-capture",
            "node_id": "worker"
        }),
    );

    assert_eq!(structured(&activated)["ok"], true);
    let pipe_calls = tmux_calls(&runner, "pipe-pane");
    assert_eq!(pipe_calls.len(), 2);
    assert_eq!(pipe_calls[1][4], "host-a:%7.%9");
    assert!(first_tmux_call_index(&runner.calls(), "pipe-pane") < runner.calls().len());
    assert!(tmux_calls(&runner, "display-message").is_empty());
    let manifest_json = read_manifest(&asset_root, "run-activate-capture");
    assert_eq!(
        manifest_json["activations"]["worker"]["preservation_status"],
        "capturing"
    );
}

#[test]
fn run_flow_rejects_readme_only_lock_without_creating_tmux_panes() {
    let asset_root = test_temp_dir("mcp-run-assets-readme-only");
    let runner = RecordingRunner::default();
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let (lock_id, content_hash) = lock_flow(
        &mut server,
        1,
        json!({
            "resources": [
                {
                    "id": "readme.main",
                    "kind": "readme",
                    "source": "inline:Distributable README-only package."
                }
            ]
        }),
    );

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-readme-only",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );

    assert_eq!(started["result"]["isError"], true);
    assert_eq!(structured(&started)["ok"], false);
    assert!(
        structured(&started)["error"]
            .as_str()
            .unwrap()
            .contains("no executable nodes")
    );
    assert!(runner.calls().is_empty());
    assert!(!asset_root.exists());
}

#[test]
fn initial_pipe_failure_blocks_same_run_id_retry_before_new_allocation() {
    let asset_root = test_temp_dir("mcp-run-assets-pipe-blocked-retry");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::failure("pipe failed"),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);

    let first = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-pipe-blocked",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );

    assert!(response_is_error(&first));
    let calls_after_first = runner.calls().len();
    let second = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-pipe-blocked",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );

    assert_eq!(second["result"]["isError"], true);
    assert_eq!(
        structured(&second)["asset_preservation"]["status"],
        "failed"
    );
    assert_eq!(runner.calls().len(), calls_after_first);
    let manifest_json = read_manifest(&asset_root, "run-pipe-blocked");
    assert_eq!(
        manifest_json["preservation_errors"][0]["stage"],
        "pipe_start"
    );
}

#[test]
fn failed_split_after_first_pane_finalizes_registered_pane_before_release() {
    let asset_root = test_temp_dir("mcp-run-assets-split-failure-cleanup");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::failure("split failed"),
        CommandOutput::success("root partial\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-split-fails",
            "nodes": ["root", "reviewer"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );

    assert!(response_is_error(&started));
    let calls = runner.calls();
    assert!(
        tmux_call_index(&calls, "capture-pane", "host-a:%7.%8")
            < tmux_call_index(&calls, "kill-pane", "host-a:%7.%8")
    );
    assert!(tmux_calls(&runner, "kill-window").is_empty());
    let manifest_json = read_manifest(&asset_root, "run-split-fails");
    assert_eq!(
        manifest_json["activations"]["root"]["termination_reason"],
        "allocation_error"
    );
}

#[test]
fn pipe_setup_failure_finalizes_failed_activation_before_release() {
    let asset_root = test_temp_dir("mcp-run-assets-pipe-setup-cleanup");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::failure("pipe failed"),
        CommandOutput::success("pipe failed final\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-pipe-setup-fails",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );

    assert!(response_is_error(&started));
    let calls = runner.calls();
    assert!(
        first_tmux_call_index(&calls, "capture-pane") < first_tmux_call_index(&calls, "kill-pane")
    );
    let manifest_json = read_manifest(&asset_root, "run-pipe-setup-fails");
    assert_eq!(
        manifest_json["activations"]["root"]["preservation_status"],
        "failed"
    );
    assert_eq!(
        manifest_json["activations"]["root"]["termination_reason"],
        "pipe_start"
    );
    assert_eq!(
        manifest_json["preservation_errors"][0]["stage"],
        "pipe_start"
    );
}

#[test]
fn applied_manifest_commit_failure_finalizes_allocated_panes() {
    let asset_root = test_temp_dir("mcp-run-assets-final-commit-cleanup");
    let runner = SideEffectRunner::with_outputs(
        vec![
            CommandOutput::failure("missing session"),
            CommandOutput::success("%7\t%8\n"),
            CommandOutput::success(""),
            CommandOutput::success("commit failure capture\n"),
            CommandOutput::success(""),
        ],
        {
            let asset_root = asset_root.clone();
            move |argv| {
                if argv.get(1).map(String::as_str) == Some("pipe-pane") {
                    let manifest_path = find_manifest_path(&asset_root, "run-final-commit-fails");
                    fs::remove_file(&manifest_path).unwrap();
                    fs::create_dir(&manifest_path).unwrap();
                }
            }
        },
    );
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-final-commit-fails",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );

    assert_eq!(started["result"]["isError"], true);
    let calls = runner.calls();
    assert!(
        first_tmux_call_index(&calls, "capture-pane") < first_tmux_call_index(&calls, "kill-pane")
    );
}

#[test]
fn failed_manifest_blocks_same_run_id_after_mcp_restart() {
    let asset_root = test_temp_dir("mcp-run-assets-restart-reuse");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::failure("pipe failed"),
        CommandOutput::success("pipe failed final\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut first_server =
        McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store.clone());

    let first = call_tool(
        &mut first_server,
        1,
        "start_run",
        json!({
            "run_id": "run-restart-blocked",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert!(response_is_error(&first));
    let calls_after_first = runner.calls().len();

    let mut second_server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let second = call_tool(
        &mut second_server,
        2,
        "start_run",
        json!({
            "run_id": "run-restart-blocked",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );

    assert_eq!(second["result"]["isError"], true);
    assert_eq!(
        structured(&second)["asset_preservation"]["status"],
        "failed"
    );
    assert_eq!(runner.calls().len(), calls_after_first);
}

#[test]
fn fanout_activation_ids_with_separators_use_authoritative_relative_paths() {
    let asset_root = test_temp_dir("mcp-run-assets-fanout-paths");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, fanout_flow());

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-fanout-paths",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-fanout-paths",
            "activation_id": "root",
            "artifact_key": "items",
            "payload": "alpha"
        }),
    );
    let resumed = call_tool(
        &mut server,
        4,
        "resume_run",
        json!({
            "run_id": "run-fanout-paths"
        }),
    );
    assert_eq!(structured(&resumed)["ok"], true);

    let manifest_json = read_manifest(&asset_root, "run-fanout-paths");
    let activation = &manifest_json["activations"]["worker:items/0"];
    assert_eq!(activation["activation_id"], "worker:items/0");
    let metadata_relative_path = activation["relative_paths"]["metadata"].as_str().unwrap();
    assert_ne!(
        metadata_relative_path,
        "activations/worker:items/0/metadata.json"
    );
    assert!(metadata_relative_path.starts_with("activations/act-sha256-"));
    assert!(metadata_relative_path.contains("worker_items_0"));
    let metadata = read_json(
        asset_root
            .join(
                run_root(&asset_root, "run-fanout-paths")
                    .file_name()
                    .unwrap(),
            )
            .join(metadata_relative_path),
    );
    assert_eq!(metadata["activation_id"], "worker:items/0");
    assert_eq!(
        metadata["relative_paths"]["metadata"],
        metadata_relative_path
    );
}

#[test]
fn public_fanout_from_artifact_allocates_and_captures_tmux_backed_activation() {
    let asset_root = test_temp_dir("mcp-run-assets-public-fanout-capture");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, fanout_flow());

    let started = call_tool(
        &mut server,
        2,
        "start_run",
        json!({
            "run_id": "run-public-fanout",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let applied = call_tool(
        &mut server,
        3,
        "apply_flow_lock",
        json!({
            "run_id": "run-public-fanout",
            "mode": "future_activations",
            "lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&applied)["ok"], true);
    call_tool(
        &mut server,
        4,
        "deliver_artifact",
        json!({
            "run_id": "run-public-fanout",
            "activation_id": "root",
            "artifact_key": "items",
            "payload": "alpha"
        }),
    );

    let fanout = call_tool(
        &mut server,
        5,
        "fanout_from_artifact",
        json!({
            "run_id": "run-public-fanout",
            "node_id": "worker",
            "artifact_key": "items",
            "for_each": "items"
        }),
    );

    assert_eq!(structured(&fanout)["ok"], true);
    assert_eq!(
        structured(&fanout)["activation_ids"],
        json!(["worker:items/0"])
    );
    let pipe_calls = tmux_calls(&runner, "pipe-pane");
    assert_eq!(pipe_calls.len(), 2);
    assert_eq!(pipe_calls[1][4], "host-a:%7.%9");
    let manifest_json = read_manifest(&asset_root, "run-public-fanout");
    let activation = &manifest_json["activations"]["worker:items/0"];
    assert_eq!(activation["activation_id"], "worker:items/0");
    assert!(
        activation["relative_paths"]["metadata"]
            .as_str()
            .unwrap()
            .starts_with("activations/act-sha256-")
    );
}

#[test]
fn stop_run_final_capture_and_manifest_update_precede_kill_for_all_tmux_panes() {
    let asset_root = test_temp_dir("mcp-run-assets-forced-stop");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
        CommandOutput::success("reviewer final\n"),
        CommandOutput::success(""),
        CommandOutput::success("root final\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-forced-stop",
            "nodes": [
                { "id": "root" },
                { "id": "reviewer" }
            ],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let stopped = call_tool(
        &mut server,
        2,
        "stop_run",
        json!({
            "run_id": "run-forced-stop"
        }),
    );

    assert_eq!(structured(&stopped)["ok"], true);
    assert_eq!(structured(&stopped)["run_status"], "stopped");
    assert_eq!(
        structured(&stopped)["tmux_cleanup"]["activations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        structured(&stopped)["tmux_cleanup"]["activations"][0]["asset_preservation"]["status"],
        "complete"
    );
    assert_eq!(
        structured(&stopped)["tmux_cleanup"]["activations"][1]["asset_preservation"]["status"],
        "complete"
    );

    let calls = runner.calls();
    let root_capture = tmux_call_index(&calls, "capture-pane", "host-a:%7.%8");
    let root_kill = tmux_call_index(&calls, "kill-pane", "host-a:%7.%8");
    let reviewer_capture = tmux_call_index(&calls, "capture-pane", "host-a:%7.%9");
    let reviewer_kill = tmux_call_index(&calls, "kill-pane", "host-a:%7.%9");
    assert!(root_capture < root_kill);
    assert!(reviewer_capture < reviewer_kill);

    let manifest_json = read_manifest(&asset_root, "run-forced-stop");
    assert_eq!(
        manifest_json["activations"]["root"]["termination_reason"],
        "forced_stop"
    );
    assert_eq!(
        manifest_json["activations"]["reviewer"]["termination_reason"],
        "forced_stop"
    );
    assert_eq!(
        fs::read_to_string(
            run_root(&asset_root, "run-forced-stop").join(
                read_manifest(&asset_root, "run-forced-stop")["activations"]["reviewer"]
                    ["relative_paths"]["final_capture"]
                    .as_str()
                    .unwrap()
            )
        )
        .unwrap(),
        "reviewer final\n"
    );
}

#[test]
fn forced_stop_preservation_failure_surfaces_and_cleanup_remains_best_effort() {
    let asset_root = test_temp_dir("mcp-run-assets-forced-stop-failure");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("root final\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-forced-failure",
            "nodes": [{ "id": "root" }],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let activation_dir = run_root(&asset_root, "run-forced-failure").join(
        read_manifest(&asset_root, "run-forced-failure")["activations"]["root"]["relative_paths"]
            ["metadata"]
            .as_str()
            .unwrap()
            .trim_end_matches("/metadata.json"),
    );
    fs::remove_dir_all(&activation_dir).unwrap();
    fs::write(&activation_dir, "not a directory").unwrap();

    let stopped = call_tool(
        &mut server,
        2,
        "stop_run",
        json!({
            "run_id": "run-forced-failure"
        }),
    );

    assert_eq!(stopped["result"]["isError"], true);
    assert_eq!(structured(&stopped)["ok"], false);
    assert_eq!(structured(&stopped)["run_status"], "failed");
    assert_eq!(
        structured(&stopped)["asset_preservation"]["status"],
        "failed"
    );
    assert_eq!(
        structured(&stopped)["tmux_cleanup"]["activations"][0]["action"],
        "kill_pane"
    );
    assert!(
        structured(&stopped)["asset_preservation"]["error"]
            .as_str()
            .unwrap()
            .contains("final-capture.txt")
    );

    let calls = runner.calls();
    let capture_index = first_tmux_call_index(&calls, "capture-pane");
    let kill_index = first_tmux_call_index(&calls, "kill-pane");
    assert!(capture_index < kill_index);

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-forced-failure"
        }),
    );
    assert_eq!(structured(&status)["context"]["run_status"], "failed");
    assert_eq!(
        structured(&status)["context"]["run_assets"]["preservation_errors"][0]["stage"],
        "final_capture"
    );
}

fn read_manifest(root: &Path, run_id: &str) -> Value {
    read_json(find_manifest_path(root, run_id))
}

fn run_root(root: &Path, run_id: &str) -> PathBuf {
    find_manifest_path(root, run_id)
        .parent()
        .unwrap()
        .to_path_buf()
}

fn find_manifest_path(root: &Path, run_id: &str) -> PathBuf {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let manifest_path = entry.path().join("manifest.json");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = read_json(&manifest_path);
        if manifest["run_id"] == run_id {
            return manifest_path;
        }
    }
    panic!(
        "manifest for {run_id} should exist below {}",
        root.display()
    );
}

fn read_json(path: impl AsRef<Path>) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn tmux_calls(runner: &RecordingRunner, command: &str) -> Vec<Vec<String>> {
    runner
        .calls()
        .into_iter()
        .filter(|call| call.get(1).map(String::as_str) == Some(command))
        .collect()
}

fn response_is_error(response: &Value) -> bool {
    response.get("error").is_some()
        || response
            .pointer("/result/isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn first_tmux_call_index(calls: &[Vec<String>], command: &str) -> usize {
    calls
        .iter()
        .position(|call| call.get(1).map(String::as_str) == Some(command))
        .unwrap_or_else(|| panic!("{command} should be called"))
}

fn tmux_call_index(calls: &[Vec<String>], command: &str, target: &str) -> usize {
    calls
        .iter()
        .position(|call| {
            call.get(1).map(String::as_str) == Some(command) && call.iter().any(|arg| arg == target)
        })
        .unwrap_or_else(|| panic!("{command} should target {target}"))
}

#[derive(Clone)]
struct SideEffectRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    outputs: Rc<RefCell<VecDeque<CommandOutput>>>,
    on_call: RunnerSideEffect,
}

type RunnerSideEffect = Rc<dyn Fn(&[String])>;

impl SideEffectRunner {
    fn with_outputs(outputs: Vec<CommandOutput>, on_call: impl Fn(&[String]) + 'static) -> Self {
        Self {
            calls: Rc::new(RefCell::new(Vec::new())),
            outputs: Rc::new(RefCell::new(outputs.into())),
            on_call: Rc::new(on_call),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

impl CommandRunner for SideEffectRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        (self.on_call)(&argv);
        let output = self.outputs.borrow_mut().pop_front().unwrap_or_default();
        if argv.get(1).map(String::as_str) == Some("pipe-pane") && output.is_success() {
            acknowledge_pipe_command(&argv);
        }
        self.calls.borrow_mut().push(argv);
        Ok(output)
    }

    fn pipe_sink_helper_is_external(&self) -> bool {
        false
    }

    fn pipe_sink_producer_closed(&self, target: &str) {
        complete_pipe_command(target);
    }
}

fn locked_agent_flow() -> Value {
    json!({
        "nodes": [
            {
                "id": "root",
                "action": {
                    "driver": "agent",
                    "prompt_ref": "prompt.start",
                    "resource_refs": ["readme.main"]
                }
            }
        ],
        "resources": [
            {
                "id": "readme.main",
                "kind": "readme",
                "source": "inline:Use Humanize to audit this library without editing files."
            },
            {
                "id": "prompt.start",
                "kind": "prompt",
                "source": "inline:Inspect the repository."
            }
        ]
    })
}

fn routed_flow() -> Value {
    json!({
        "nodes": [
            { "id": "root" },
            {
                "id": "finish",
                "contract_id": "contract.finish",
                "action": {
                    "driver": "agent"
                }
            }
        ],
        "contracts": [
            {
                "id": "contract.finish",
                "completion": "all_artifacts",
                "artifacts": [
                    {
                        "id": "done",
                        "schema_resource_id": "schema.done"
                    }
                ]
            }
        ],
        "resources": [
            {
                "id": "readme.main",
                "kind": "readme",
                "source": "inline:Use Humanize to audit this library without editing files."
            },
            {
                "id": "schema.done",
                "kind": "schema",
                "source": "inline:done"
            }
        ],
        "routes": [
            {
                "predicate": "exists(artifact.ready)",
                "activate": "finish"
            }
        ]
    })
}

fn dynamic_agent_flow() -> Value {
    json!({
        "nodes": [
            { "id": "root" },
            {
                "id": "worker",
                "action": {
                    "driver": "agent"
                }
            }
        ],
        "resources": [
            {
                "id": "readme.main",
                "kind": "readme",
                "source": "inline:Use Humanize to allocate worker activations without editing files."
            }
        ]
    })
}

fn fanout_flow() -> Value {
    json!({
        "nodes": [
            { "id": "root" },
            {
                "id": "worker",
                "contract_id": "contract.worker",
                "action": {
                    "driver": "agent"
                }
            }
        ],
        "contracts": [
            {
                "id": "contract.worker",
                "completion": "all_artifacts",
                "artifacts": [
                    {
                        "id": "done",
                        "schema_resource_id": "schema.done"
                    }
                ]
            }
        ],
        "resources": [
            {
                "id": "readme.main",
                "kind": "readme",
                "source": "inline:Use Humanize to fan out work without editing files."
            },
            {
                "id": "schema.done",
                "kind": "schema",
                "source": "inline:done"
            }
        ],
        "routes": [
            {
                "predicate": "exists(artifact.items)",
                "for_each": "artifact.items",
                "activate": "worker"
            }
        ]
    })
}
