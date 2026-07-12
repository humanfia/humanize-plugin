mod support;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use humanize_plugin::adapters::tmux::{CommandOutput, CommandRunner, TmuxError};
use humanize_plugin::mcp::McpServer;
use humanize_plugin::run_assets::{RunAssetFaultPoint, RunAssetSink, RunAssetStore};
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
fn unlocked_dynamic_activation_is_registered_and_captured_for_tmux_run() {
    let asset_root = test_temp_dir("mcp-assets-unlocked-dynamic-capture");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-unlocked-dynamic",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let activated = call_tool(
        &mut server,
        2,
        "activate_node",
        json!({
            "run_id": "run-unlocked-dynamic",
            "node_id": "dynamic"
        }),
    );

    assert_eq!(structured(&activated)["ok"], true);
    assert_eq!(
        structured(&activated)["tmux_allocations"][0]["activation_id"],
        "dynamic"
    );
    let manifest = read_manifest(&asset_root, "run-unlocked-dynamic");
    assert_eq!(
        manifest["activations"]["dynamic"]["preservation_status"],
        "capturing"
    );
    assert_eq!(manifest["activations"]["dynamic"]["pane_id"], "%9");
}

#[test]
fn routed_plain_activation_is_captured_when_tmux_is_allocated() {
    let asset_root = test_temp_dir("mcp-assets-routed-plain-capture");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(
        &mut server,
        1,
        json!({
            "nodes": [
                { "id": "root" },
                { "id": "finish" }
            ],
            "resources": [support::mcp::readme_resource()],
            "routes": [
                {
                    "predicate": "exists(artifact.ready)",
                    "activate": "finish"
                }
            ]
        }),
    );
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-routed-plain",
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
    let delivered = call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": "run-routed-plain",
            "activation_id": "root",
            "artifact_key": "ready",
            "payload": "true"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);

    let resumed = call_tool(
        &mut server,
        4,
        "resume_run",
        json!({ "run_id": "run-routed-plain" }),
    );

    assert_eq!(structured(&resumed)["ok"], true);
    assert_eq!(
        structured(&resumed)["tmux_allocations"][0]["activation_id"],
        "finish"
    );
    let manifest = read_manifest(&asset_root, "run-routed-plain");
    assert_eq!(
        manifest["activations"]["finish"]["preservation_status"],
        "capturing"
    );
}

#[test]
fn nodes_only_ids_are_injective_and_match_live_stop_requirements() {
    let asset_root = test_temp_dir("mcp-assets-nodes-only-id-collision");
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server =
        McpServer::with_tmux_runner_and_run_asset_store(RecordingRunner::default(), store);

    let started = call_tool(
        &mut server,
        1,
        "run_flow",
        json!({
            "run_id": "run-nodes-only-id-collision",
            "nodes": [
                {
                    "id": "a/b",
                    "required_artifacts": ["stop/item"]
                },
                {
                    "id": "a_b",
                    "required_artifacts": ["stop_item"]
                }
            ]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let status = call_tool(
        &mut server,
        2,
        "run_status",
        json!({ "run_id": "run-nodes-only-id-collision" }),
    );
    assert_eq!(
        structured(&status)["context"]["missing_stop_contracts"]["a/b"],
        json!(["artifact:stop/item"])
    );
    assert_eq!(
        structured(&status)["context"]["missing_stop_contracts"]["a_b"],
        json!(["artifact:stop_item"])
    );

    let manifest = read_manifest(&asset_root, "run-nodes-only-id-collision");
    let export_relative = manifest["flow"]["current_export_relative_path"]
        .as_str()
        .unwrap();
    let export: Value = serde_json::from_str(
        &fs::read_to_string(
            run_root(&asset_root, "run-nodes-only-id-collision").join(export_relative),
        )
        .unwrap(),
    )
    .unwrap();
    let canonical: Value = serde_json::from_str(export["content"].as_str().unwrap()).unwrap();
    let contracts = canonical["node_contracts"].as_array().unwrap();
    assert_eq!(contracts.len(), 2);
    assert_ne!(contracts[0]["contract_id"], contracts[1]["contract_id"]);
    assert_ne!(
        contracts[0]["artifact_requirements"][0]["schema_resource_id"],
        contracts[1]["artifact_requirements"][0]["schema_resource_id"]
    );
    assert_eq!(
        contracts
            .iter()
            .map(|contract| {
                (
                    contract["node_id"].as_str().unwrap(),
                    contract["artifact_requirements"][0]["id"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>(),
        vec![("a/b", "stop/item"), ("a_b", "stop_item")]
    );
}

#[test]
fn activate_node_split_failure_retains_failed_expected_activation() {
    let asset_root = test_temp_dir("mcp-assets-activate-split-failure");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::failure("split failed"),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, dynamic_agent_flow());
    start_and_apply_dynamic_flow(&mut server, &lock_id, &content_hash, "run-activate-split");

    let activated = call_tool(
        &mut server,
        4,
        "activate_node",
        json!({
            "run_id": "run-activate-split",
            "node_id": "worker"
        }),
    );

    assert!(response_is_error(&activated));
    let manifest = read_manifest(&asset_root, "run-activate-split");
    assert_eq!(manifest["preservation_blocked"], true);
    assert_eq!(manifest["activations"]["worker"]["activation_id"], "worker");
    assert_eq!(manifest["activations"]["worker"]["node_id"], "worker");
    assert_eq!(
        manifest["activations"]["worker"]["preservation_status"],
        "failed"
    );
    assert_eq!(manifest["activations"]["worker"]["capture_complete"], false);
    assert!(
        manifest["completion"]["incomplete_tmux_activations"]
            .as_array()
            .unwrap()
            .contains(&json!("worker"))
    );
    assert_eq!(
        manifest["preservation_errors"][0]["activation_id"],
        "worker"
    );
}

#[test]
fn fanout_split_failure_represents_each_expected_activation() {
    let asset_root = test_temp_dir("mcp-assets-fanout-split-failure");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::failure("split failed"),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, fanout_flow());
    start_and_apply_dynamic_flow(&mut server, &lock_id, &content_hash, "run-fanout-split");
    deliver_items(&mut server, "run-fanout-split", "alpha\nbeta");

    let fanout = call_tool(
        &mut server,
        5,
        "fanout_from_artifact",
        json!({
            "run_id": "run-fanout-split",
            "node_id": "worker",
            "artifact_key": "items",
            "for_each": "items"
        }),
    );

    assert!(response_is_error(&fanout));
    let manifest = read_manifest(&asset_root, "run-fanout-split");
    assert_eq!(manifest["preservation_blocked"], true);
    assert_eq!(
        manifest["activations"]["worker:items/0"]["preservation_status"],
        "failed"
    );
    assert_eq!(
        manifest["activations"]["worker:items/1"]["preservation_status"],
        "pending"
    );
    assert_eq!(
        manifest["completion"]["incomplete_tmux_activations"],
        json!(["root", "worker:items/0", "worker:items/1"])
    );
}

#[test]
fn activate_node_returns_error_after_capture_failure_even_when_replacement_captures() {
    let asset_root = test_temp_dir("mcp-assets-activate-replacement-blocked");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::failure("pipe failed"),
        CommandOutput::success("failed worker final\n"),
        CommandOutput::success(""),
        CommandOutput::success("%10\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, dynamic_agent_flow());
    start_and_apply_dynamic_flow(&mut server, &lock_id, &content_hash, "run-activate-blocked");

    let activated = call_tool(
        &mut server,
        4,
        "activate_node",
        json!({
            "run_id": "run-activate-blocked",
            "node_id": "worker"
        }),
    );

    assert!(response_is_error(&activated));
    let manifest = read_manifest(&asset_root, "run-activate-blocked");
    assert_eq!(manifest["preservation_blocked"], true);
    assert_eq!(
        manifest["preservation_errors"][0]["activation_id"],
        "worker"
    );
}

#[test]
fn fanout_returns_error_after_capture_failure_even_when_replacement_captures() {
    let asset_root = test_temp_dir("mcp-assets-fanout-replacement-blocked");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::failure("pipe failed"),
        CommandOutput::success("failed fanout final\n"),
        CommandOutput::success(""),
        CommandOutput::success("%10\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, fanout_flow());
    start_and_apply_dynamic_flow(&mut server, &lock_id, &content_hash, "run-fanout-blocked");
    deliver_items(&mut server, "run-fanout-blocked", "alpha");

    let fanout = call_tool(
        &mut server,
        5,
        "fanout_from_artifact",
        json!({
            "run_id": "run-fanout-blocked",
            "node_id": "worker",
            "artifact_key": "items",
            "for_each": "items"
        }),
    );

    assert!(response_is_error(&fanout));
    let manifest = read_manifest(&asset_root, "run-fanout-blocked");
    assert_eq!(manifest["preservation_blocked"], true);
    assert_eq!(
        manifest["preservation_errors"][0]["activation_id"],
        "worker:items/0"
    );
}

#[test]
fn stop_run_preserves_prior_asset_failure_status_after_cleanup() {
    let asset_root = test_temp_dir("mcp-assets-stop-prior-failure");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::failure("split failed"),
        CommandOutput::success("root final\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, dynamic_agent_flow());
    start_and_apply_dynamic_flow(&mut server, &lock_id, &content_hash, "run-stop-failed");
    let activated = call_tool(
        &mut server,
        4,
        "activate_node",
        json!({
            "run_id": "run-stop-failed",
            "node_id": "worker"
        }),
    );
    assert!(response_is_error(&activated));

    let stopped = call_tool(
        &mut server,
        5,
        "stop_run",
        json!({
            "run_id": "run-stop-failed"
        }),
    );

    assert_eq!(stopped["result"]["isError"], true);
    assert_eq!(structured(&stopped)["ok"], false);
    assert_eq!(structured(&stopped)["run_status"], "failed");
    assert_eq!(
        structured(&stopped)["asset_preservation"]["status"],
        "failed"
    );
    let manifest = read_manifest(&asset_root, "run-stop-failed");
    assert_eq!(manifest["preservation_blocked"], true);
    assert_eq!(
        manifest["activations"]["worker"]["preservation_status"],
        "failed"
    );
}

#[test]
fn activation_store_faults_mark_exact_activation_failed_and_incomplete() {
    for (name, fault, expected_phase, pipe_created) in [
        (
            "register",
            RunAssetFaultPoint::RegisterExpectedActivation,
            "failed",
            false,
        ),
        (
            "transcript",
            RunAssetFaultPoint::StartActivationTranscript,
            "failed",
            false,
        ),
        (
            "metadata",
            RunAssetFaultPoint::StartActivationMetadata,
            "failed",
            true,
        ),
        (
            "manifest",
            RunAssetFaultPoint::StartActivationManifest,
            "failed",
            true,
        ),
    ] {
        let asset_root = test_temp_dir(&format!("mcp-assets-store-fault-{name}"));
        let runner = RecordingRunner::with_outputs(vec![
            CommandOutput::failure("missing session"),
            CommandOutput::success("%7\t%8\n"),
            CommandOutput::success(""),
            CommandOutput::success("%9\n"),
        ]);
        let store = RunAssetStore::new_with_fault(
            RunAssetSink::Root(asset_root.clone()),
            fault.for_activation("worker"),
        );
        let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
        let (lock_id, content_hash) = lock_flow(&mut server, 1, dynamic_agent_flow());
        start_and_apply_dynamic_flow(&mut server, &lock_id, &content_hash, "run-store-fault");

        let activated = call_tool(
            &mut server,
            4,
            "activate_node",
            json!({
                "run_id": "run-store-fault",
                "node_id": "worker"
            }),
        );

        assert!(response_is_error(&activated));
        let manifest = read_manifest(&asset_root, "run-store-fault");
        let activation = &manifest["activations"]["worker"];
        assert_eq!(manifest["preservation_blocked"], true);
        assert_eq!(activation["activation_id"], "worker");
        assert_eq!(activation["node_id"], "worker");
        assert_eq!(activation["capture_phase"], expected_phase);
        assert_eq!(activation["pipe_acknowledged"], false);
        assert_eq!(activation["capture_complete"], false);
        assert_eq!(activation["preservation_status"], "failed");
        assert!(
            manifest["completion"]["incomplete_tmux_activations"]
                .as_array()
                .unwrap()
                .contains(&json!("worker"))
        );
        assert_eq!(
            manifest["preservation_errors"][0]["activation_id"],
            "worker"
        );
        assert_eq!(manifest["completion"]["complete"], false);
        let pipe_relative = activation["relative_paths"]["transcript_pipe"]
            .as_str()
            .unwrap();
        assert_eq!(
            run_root(&asset_root, "run-store-fault")
                .join(pipe_relative)
                .exists(),
            pipe_created
        );
    }
}

#[test]
fn failed_starting_activation_cleanup_does_not_upgrade_to_complete() {
    let asset_root = test_temp_dir("mcp-assets-starting-cleanup-remains-failed");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success("starting final\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new_with_fault(
        RunAssetSink::Root(asset_root.clone()),
        RunAssetFaultPoint::StartActivationManifest.for_activation("worker"),
    );
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, dynamic_agent_flow());
    start_and_apply_dynamic_flow(&mut server, &lock_id, &content_hash, "run-starting-cleanup");
    let activated = call_tool(
        &mut server,
        4,
        "activate_node",
        json!({
            "run_id": "run-starting-cleanup",
            "node_id": "worker"
        }),
    );
    assert!(response_is_error(&activated));

    let stopped = call_tool(
        &mut server,
        5,
        "stop_run",
        json!({
            "run_id": "run-starting-cleanup"
        }),
    );

    assert!(response_is_error(&stopped));
    let manifest = read_manifest(&asset_root, "run-starting-cleanup");
    let activation = &manifest["activations"]["worker"];
    assert_eq!(activation["capture_phase"], "failed");
    assert_eq!(activation["pipe_acknowledged"], false);
    assert_eq!(activation["capture_complete"], false);
    assert_eq!(activation["preservation_status"], "failed");
    assert_eq!(manifest["completion"]["complete"], false);
}

#[test]
fn pipe_setup_failure_redacts_nonce_and_command_from_api_and_manifest() {
    let asset_root = test_temp_dir("mcp-assets-pipe-redaction");
    let runner = EchoPipeFailureRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success("%9\n"),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, dynamic_agent_flow());
    start_and_apply_dynamic_flow(&mut server, &lock_id, &content_hash, "run-redacted-pipe");

    let activated = call_tool(
        &mut server,
        4,
        "activate_node",
        json!({
            "run_id": "run-redacted-pipe",
            "node_id": "worker"
        }),
    );

    assert!(response_is_error(&activated));
    let command = runner.pipe_commands().pop().unwrap();
    let nonce = shell_arg_after(&command, "--ack-nonce").unwrap();
    let response_text = serde_json::to_string(&activated).unwrap();
    let manifest = read_manifest(&asset_root, "run-redacted-pipe");
    let manifest_text = serde_json::to_string(&manifest).unwrap();
    for text in [response_text.as_str(), manifest_text.as_str()] {
        assert!(!text.contains("--ack-nonce"));
        assert!(!text.contains(&nonce));
        assert!(!text.contains(&command));
    }
    assert_eq!(
        manifest["preservation_errors"][0]["error"],
        "tmux pipe-pane -o -t host-a:%7.%9 <pipe-sink-command-redacted> failed with status 1: pipe sink setup failed"
    );
}

fn start_and_apply_dynamic_flow<R: CommandRunner>(
    server: &mut McpServer<R>,
    lock_id: &str,
    content_hash: &str,
    run_id: &str,
) {
    let started = call_tool(
        server,
        2,
        "start_run",
        json!({
            "run_id": run_id,
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
        server,
        3,
        "apply_flow_lock",
        json!({
            "run_id": run_id,
            "mode": "future_activations",
            "lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    assert_eq!(structured(&applied)["ok"], true);
}

fn deliver_items(server: &mut McpServer<RecordingRunner>, run_id: &str, payload: &str) {
    let delivered = call_tool(
        server,
        4,
        "deliver_artifact",
        json!({
            "run_id": run_id,
            "activation_id": "root",
            "artifact_key": "items",
            "payload": payload
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);
}

fn response_is_error(response: &Value) -> bool {
    response.get("error").is_some()
        || response
            .pointer("/result/isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn read_manifest(root: &Path, run_id: &str) -> Value {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let manifest_path = entry.path().join("manifest.json");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        if manifest["run_id"] == run_id {
            return manifest;
        }
    }
    panic!(
        "manifest for {run_id} should exist below {}",
        root.display()
    );
}

fn run_root(root: &Path, run_id: &str) -> PathBuf {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let manifest_path = entry.path().join("manifest.json");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        if manifest["run_id"] == run_id {
            return entry.path();
        }
    }
    panic!(
        "run root for {run_id} should exist below {}",
        root.display()
    );
}

#[derive(Clone, Default)]
struct EchoPipeFailureRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    outputs: Rc<RefCell<VecDeque<CommandOutput>>>,
    pipe_commands: Rc<RefCell<Vec<String>>>,
    pipe_count: Rc<RefCell<usize>>,
}

impl EchoPipeFailureRunner {
    fn with_outputs(outputs: Vec<CommandOutput>) -> Self {
        Self {
            calls: Rc::new(RefCell::new(Vec::new())),
            outputs: Rc::new(RefCell::new(outputs.into())),
            pipe_commands: Rc::new(RefCell::new(Vec::new())),
            pipe_count: Rc::new(RefCell::new(0)),
        }
    }

    fn pipe_commands(&self) -> Vec<String> {
        self.pipe_commands.borrow().clone()
    }
}

impl CommandRunner for EchoPipeFailureRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        if argv.get(1).map(String::as_str) == Some("pipe-pane") {
            let mut pipe_count = self.pipe_count.borrow_mut();
            *pipe_count += 1;
            if *pipe_count == 1 {
                acknowledge_pipe_command(&argv);
                self.calls.borrow_mut().push(argv);
                return Ok(CommandOutput::success(""));
            }
            let command = argv.get(5).cloned().unwrap_or_default();
            self.pipe_commands.borrow_mut().push(command.clone());
            self.calls.borrow_mut().push(argv);
            return Ok(CommandOutput::failure(format!(
                "wrapper echoed helper command: {command}"
            )));
        }
        self.calls.borrow_mut().push(argv);
        Ok(self.outputs.borrow_mut().pop_front().unwrap_or_default())
    }

    fn pipe_sink_helper_is_external(&self) -> bool {
        false
    }

    fn pipe_sink_producer_closed(&self, target: &str) {
        complete_pipe_command(target);
    }
}

fn shell_arg_after(command: &str, flag: &str) -> Option<String> {
    let rest = command.split_once(flag)?.1.trim_start();
    if let Some(rest) = rest.strip_prefix('\'') {
        let value = rest.split('\'').next()?;
        return Some(value.to_string());
    }
    rest.split_whitespace().next().map(str::to_string)
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
