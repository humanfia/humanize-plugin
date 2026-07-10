mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use humanize_plugin::adapters::tmux::{CommandOutput, CommandRunner, TmuxError};
use humanize_plugin::mcp::McpServer;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use support::mcp::{RecordingRunner, call_tool, lock_flow, structured};

#[derive(Clone)]
struct MissingCompletionRunner {
    inner: RecordingRunner,
}

impl CommandRunner for MissingCompletionRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        self.inner.run(argv)
    }

    fn pipe_sink_helper_is_external(&self) -> bool {
        false
    }
}

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
fn stop_run_surfaces_kill_failure_and_retry_only_releases_finalized_pane() {
    let asset_root = test_temp_dir("mcp-run-assets-stop-kill-retry");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("root final\n"),
        CommandOutput::failure("kill failed"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-stop-kill-retry",
            "nodes": [{ "id": "root" }],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let first_started_at = Instant::now();
    let first_stop = call_tool(
        &mut server,
        2,
        "stop_run",
        json!({ "run_id": "run-stop-kill-retry" }),
    );
    assert!(first_started_at.elapsed() < Duration::from_millis(500));

    assert_eq!(first_stop["result"]["isError"], true);
    assert_eq!(structured(&first_stop)["ok"], false);
    assert_eq!(structured(&first_stop)["run_status"], "failed");
    assert_eq!(
        structured(&first_stop)["tmux_cleanup"]["cleanup_errors"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let failed_manifest = read_manifest(&asset_root, "run-stop-kill-retry");
    let failed_activation = &failed_manifest["activations"]["root"];
    assert_eq!(failed_activation["capture_phase"], "capturing");
    assert_eq!(failed_activation["capture_complete"], false);
    assert_eq!(failed_activation["preservation_status"], "capturing");
    assert_eq!(failed_activation["resource_cleanup_status"], "failed");
    assert_eq!(failed_manifest["preservation_errors"], json!([]));
    let failed_metadata = read_json(
        run_root(&asset_root, "run-stop-kill-retry").join(
            failed_activation["relative_paths"]["metadata"]
                .as_str()
                .unwrap(),
        ),
    );
    assert_eq!(failed_metadata["capture_complete"], false);
    assert_eq!(failed_metadata["preservation_status"], "capturing");
    assert_eq!(failed_metadata["resource_cleanup_status"], "failed");

    let second_stop = call_tool(
        &mut server,
        3,
        "stop_run",
        json!({ "run_id": "run-stop-kill-retry" }),
    );

    assert_eq!(structured(&second_stop)["ok"], true);
    assert_eq!(structured(&second_stop)["run_status"], "stopped");
    assert_eq!(
        structured(&second_stop)["tmux_cleanup"]["cleanup_errors"],
        json!([])
    );
    let calls = runner.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("capture-pane"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("kill-pane"))
            .count(),
        2
    );
    let completed_manifest = read_manifest(&asset_root, "run-stop-kill-retry");
    let completed_activation = &completed_manifest["activations"]["root"];
    assert_eq!(completed_activation["capture_phase"], "complete");
    assert_eq!(completed_activation["capture_complete"], true);
    assert_eq!(completed_activation["preservation_status"], "complete");
    assert_eq!(completed_activation["resource_cleanup_status"], "complete");
    assert_eq!(completed_manifest["preservation_errors"], json!([]));
    let completed_metadata = read_json(
        run_root(&asset_root, "run-stop-kill-retry").join(
            completed_activation["relative_paths"]["metadata"]
                .as_str()
                .unwrap(),
        ),
    );
    assert_eq!(completed_metadata["capture_complete"], true);
    assert_eq!(completed_metadata["resource_cleanup_status"], "complete");
}

#[test]
fn stop_run_surfaces_all_tmux_release_failures() {
    let asset_root = test_temp_dir("mcp-run-assets-stop-multi-kill-failure");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
        CommandOutput::success("reviewer final\n"),
        CommandOutput::failure("reviewer kill failed"),
        CommandOutput::success("root final\n"),
        CommandOutput::failure("root kill failed"),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-stop-multi-kill-failure",
            "nodes": [{ "id": "root" }, { "id": "reviewer" }],
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
        json!({ "run_id": "run-stop-multi-kill-failure" }),
    );

    assert_eq!(stopped["result"]["isError"], true);
    assert_eq!(structured(&stopped)["ok"], false);
    assert_eq!(structured(&stopped)["run_status"], "failed");
    let cleanup_errors = structured(&stopped)["tmux_cleanup"]["cleanup_errors"]
        .as_array()
        .unwrap();
    assert_eq!(cleanup_errors.len(), 2);
    assert_eq!(
        cleanup_errors
            .iter()
            .map(|error| error["activation_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["reviewer", "root"]
    );
    let manifest = read_manifest(&asset_root, "run-stop-multi-kill-failure");
    for activation_id in ["root", "reviewer"] {
        assert_eq!(
            manifest["activations"][activation_id]["capture_complete"],
            false
        );
        assert_eq!(
            manifest["activations"][activation_id]["preservation_status"],
            "capturing"
        );
        assert_eq!(
            manifest["activations"][activation_id]["resource_cleanup_status"],
            "failed"
        );
    }
}

#[test]
fn failed_kill_surfaces_cleanup_metadata_persistence_failure_and_recovers() {
    let asset_root = test_temp_dir("mcp-run-assets-kill-and-metadata-failure");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("root final\n"),
        CommandOutput::failure("kill failed"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new_with_resource_cleanup_fault_once(
        RunAssetSink::Root(asset_root.clone()),
        "root",
    );
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-kill-and-metadata-failure",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let first_stop = call_tool(
        &mut server,
        2,
        "stop_run",
        json!({ "run_id": "run-kill-and-metadata-failure" }),
    );

    assert_eq!(first_stop["result"]["isError"], true);
    let cleanup_error = &structured(&first_stop)["tmux_cleanup"]["cleanup_errors"][0];
    assert_eq!(cleanup_error["stage"], "kill_pane");
    assert_eq!(
        cleanup_error["resource_cleanup_persistence"]["stage"],
        "resource_cleanup_manifest"
    );
    let failed_manifest = read_manifest(&asset_root, "run-kill-and-metadata-failure");
    assert_eq!(
        failed_manifest["activations"]["root"]["preservation_status"],
        "capturing"
    );
    assert_eq!(
        failed_manifest["activations"]["root"]["resource_cleanup_status"],
        "pending"
    );

    let second_stop = call_tool(
        &mut server,
        3,
        "stop_run",
        json!({ "run_id": "run-kill-and-metadata-failure" }),
    );

    assert_eq!(structured(&second_stop)["ok"], true);
    let completed_manifest = read_manifest(&asset_root, "run-kill-and-metadata-failure");
    assert_eq!(
        completed_manifest["activations"]["root"]["preservation_status"],
        "complete"
    );
    assert_eq!(
        completed_manifest["activations"]["root"]["resource_cleanup_status"],
        "complete"
    );
    let calls = runner.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("capture-pane"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("kill-pane"))
            .count(),
        2
    );
}

#[test]
fn stop_run_rejects_capture_without_durable_pipe_completion() {
    let asset_root = test_temp_dir("mcp-run-assets-missing-pipe-completion");
    let inner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("final pane capture\n"),
        CommandOutput::success(""),
    ]);
    let runner = MissingCompletionRunner {
        inner: inner.clone(),
    };
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-missing-pipe-completion",
            "nodes": [{ "id": "root" }],
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
        json!({ "run_id": "run-missing-pipe-completion" }),
    );

    assert_eq!(stopped["result"]["isError"], true);
    assert_eq!(structured(&stopped)["ok"], false);
    let manifest = read_manifest(&asset_root, "run-missing-pipe-completion");
    assert_eq!(manifest["activations"]["root"]["capture_complete"], false);
    assert_eq!(
        manifest["activations"]["root"]["preservation_status"],
        "failed"
    );
    assert_eq!(
        manifest["activations"]["root"]["resource_cleanup_status"],
        "complete"
    );
    assert_eq!(manifest["preservation_blocked"], true);
}

#[test]
fn cleanup_metadata_failure_retains_released_pane_for_metadata_only_retry() {
    let asset_root = test_temp_dir("mcp-run-assets-cleanup-metadata-retry");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("final pane capture\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new_with_resource_cleanup_fault_once(
        RunAssetSink::Root(asset_root.clone()),
        "root",
    );
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-cleanup-metadata-retry",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let first_stop = call_tool(
        &mut server,
        2,
        "stop_run",
        json!({ "run_id": "run-cleanup-metadata-retry" }),
    );
    assert_eq!(first_stop["result"]["isError"], true);
    assert_eq!(structured(&first_stop)["ok"], false);

    let second_stop = call_tool(
        &mut server,
        3,
        "stop_run",
        json!({ "run_id": "run-cleanup-metadata-retry" }),
    );
    assert_eq!(structured(&second_stop)["ok"], true);
    let calls = runner.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("capture-pane"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("kill-pane"))
            .count(),
        1
    );
    let manifest = read_manifest(&asset_root, "run-cleanup-metadata-retry");
    assert_eq!(
        manifest["activations"]["root"]["preservation_status"],
        "complete"
    );
    assert_eq!(
        manifest["activations"]["root"]["resource_cleanup_status"],
        "complete"
    );
}

#[test]
fn start_run_rollback_surfaces_pane_release_failure() {
    let asset_root = test_temp_dir("mcp-run-assets-start-rollback-release-failure");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::failure("pipe setup failed"),
        CommandOutput::success("final pane capture\n"),
        CommandOutput::failure("kill failed"),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);

    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-start-rollback-release-failure",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );

    assert_eq!(started["result"]["isError"], true);
    let error = structured(&started)["error"].as_str().unwrap();
    assert!(error.contains("pipe sink setup failed"));
    assert!(error.contains("tmux resource cleanup failed"));
}

#[test]
fn run_flow_rollback_surfaces_pane_release_failure() {
    let asset_root = test_temp_dir("mcp-run-assets-flow-rollback-release-failure");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::failure("pipe setup failed"),
        CommandOutput::success("final pane capture\n"),
        CommandOutput::failure("kill failed"),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(
        &mut server,
        1,
        json!({
            "nodes": ["root"],
            "resources": [support::mcp::readme_resource()]
        }),
    );

    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-flow-rollback-release-failure",
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
    let error = structured(&started)["error"].as_str().unwrap();
    assert!(error.contains("pipe sink setup failed"));
    assert!(error.contains("tmux resource cleanup failed"));
}

#[test]
fn shutdown_retries_released_pane_cleanup_metadata_without_refinalizing() {
    let asset_root = test_temp_dir("mcp-run-assets-shutdown-metadata-retry");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("final pane capture\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new_with_resource_cleanup_fault_once(
        RunAssetSink::Root(asset_root.clone()),
        "root",
    );
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-shutdown-metadata-retry",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let first = server
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "shutdown"
        }))
        .unwrap();
    let second = server
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "shutdown"
        }))
        .unwrap();

    assert_eq!(first["result"]["ok"], false);
    assert_eq!(second["result"]["ok"], true);
    let calls = runner.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("capture-pane"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("kill-pane"))
            .count(),
        1
    );
    let manifest = read_manifest(&asset_root, "run-shutdown-metadata-retry");
    assert_eq!(
        manifest["activations"]["root"]["resource_cleanup_status"],
        "complete"
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
