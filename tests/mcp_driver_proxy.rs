mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::driver::socket_path_for_run_root;
use humanize_plugin::flow::{
    self, ContractArtifact, ContractCompletion, FlowCheckMode, FlowContract, FlowDraft, FlowLock,
    FlowNode, FlowPolicies, FlowResource, ResourceKind,
};
use humanize_plugin::mcp::McpServer;
use humanize_plugin::review::{ReviewDecision, ReviewStore};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use support::mcp::{RecordingRunner, call_tool, lock_flow, structured};

#[test]
fn two_mcp_servers_proxy_existing_run_to_one_driver() {
    let fixture = DriverFixture::new("mcp-driver-shared");
    let mut driver = fixture.spawn("run-shared");
    assert_eq!(
        fixture.request("run-shared", bind_run_request("bind", "run-shared"))["ok"],
        true
    );

    let mut server_a = fixture.mcp_server();
    let mut server_b = fixture.mcp_server();

    let status_a = call_tool(
        &mut server_a,
        1,
        "run_status",
        json!({
            "run_id": "run-shared"
        }),
    );
    assert_eq!(structured(&status_a)["ok"], true);
    assert_eq!(structured(&status_a)["run_status"], "running");

    let delivered = call_tool(
        &mut server_a,
        2,
        "deliver_artifact",
        json!({
            "run_id": "run-shared",
            "activation_id": "root",
            "artifact_key": "brief",
            "payload": "from-server-a"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true);

    let status_b = call_tool(
        &mut server_b,
        3,
        "run_status",
        json!({
            "run_id": "run-shared"
        }),
    );
    assert_eq!(
        structured(&status_b)["context"]["artifacts"]["brief"],
        "from-server-a"
    );
    driver.shutdown();
}

#[test]
fn restarted_mcp_process_reconnects_to_existing_driver_state() {
    let fixture = DriverFixture::new("mcp-driver-restart");
    let mut driver = fixture.spawn("run-reconnect");
    assert_eq!(
        fixture.request("run-reconnect", bind_run_request("bind", "run-reconnect"))["ok"],
        true
    );

    {
        let mut first_server = fixture.mcp_server();
        let paused = call_tool(
            &mut first_server,
            1,
            "pause_run",
            json!({
                "run_id": "run-reconnect"
            }),
        );
        assert_eq!(structured(&paused)["run_status"], "paused");
    }

    let mut restarted_server = fixture.mcp_server();
    let status = call_tool(
        &mut restarted_server,
        2,
        "run_status",
        json!({
            "run_id": "run-reconnect"
        }),
    );
    assert_eq!(structured(&status)["ok"], true);
    assert_eq!(structured(&status)["run_status"], "paused");
    driver.shutdown();
}

#[test]
fn run_flow_reattach_rejects_mode_or_initial_limit_conflict_before_local_lock_lookup() {
    let fixture = DriverFixture::new("mcp-driver-run-config");
    let mut driver = fixture.spawn("run-config");
    let mut bind = bind_run_request("bind", "run-config");
    bind["run_mode"] = json!("continuous");
    bind["activation_limit"] = json!(3);
    assert_eq!(fixture.request("run-config", bind)["ok"], true);
    let mut server = fixture.mcp_server();

    let conflict = call_tool(
        &mut server,
        1,
        "run_flow",
        json!({ "run_id": "run-config" }),
    );
    assert_eq!(
        structured(&conflict)["error"]["code"],
        "run_binding_conflict",
        "{conflict}"
    );

    let attached = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-config",
            "runMode": "continuous",
            "activationLimit": 3
        }),
    );
    assert_eq!(structured(&attached)["ok"], true, "{attached}");
    assert_eq!(structured(&attached)["attached"], true, "{attached}");
    assert_eq!(structured(&attached)["run_mode"], "continuous");
    assert_eq!(structured(&attached)["initial_activation_limit"], 3);

    let (other_lock_id, other_content_hash) = lock_flow(
        &mut server,
        3,
        json!({
            "nodes": [{ "id": "other" }],
            "resources": [{
                "path": "README.md",
                "kind": "readme",
                "content": "Conflicting live attach flow."
            }]
        }),
    );
    let lock_conflict = call_tool(
        &mut server,
        4,
        "run_flow",
        json!({
            "run_id": "run-config",
            "run_mode": "continuous",
            "activation_limit": 3,
            "flow_lock_id": other_lock_id,
            "content_hash": other_content_hash,
        }),
    );
    assert_eq!(
        structured(&lock_conflict)["error"]["code"],
        "run_binding_conflict",
        "{lock_conflict}"
    );
    driver.shutdown();
}

#[test]
fn complete_run_proxies_and_only_completes_a_quiescent_manual_run() {
    let fixture = DriverFixture::new("mcp-driver-complete-run");
    let mut driver = fixture.spawn("run-complete");
    let mut bind = bind_run_request("bind", "run-complete");
    bind["run_mode"] = json!("manual");
    bind["activation_limit"] = json!(2);
    assert_eq!(fixture.request("run-complete", bind)["ok"], true);
    let mut server = fixture.mcp_server();

    let early = call_tool(
        &mut server,
        1,
        "complete_run",
        json!({ "run_id": "run-complete" }),
    );
    assert_eq!(structured(&early)["ok"], false, "{early}");

    let delivered = call_tool(
        &mut server,
        2,
        "deliver_artifact",
        json!({
            "run_id": "run-complete",
            "activation_id": "root",
            "artifact_key": "brief",
            "payload": "done"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true, "{delivered}");
    let stopped = fixture.request(
        "run-complete",
        json!({
            "id": "observe-root-stop",
            "op": "observe_stop",
            "run_id": "run-complete",
            "activation_id": "root",
            "reason": "done"
        }),
    );
    assert_eq!(stopped["run_status"], "quiescent", "{stopped}");

    let completed = call_tool(
        &mut server,
        4,
        "complete_run",
        json!({ "run_id": "run-complete" }),
    );
    assert_eq!(structured(&completed)["ok"], true, "{completed}");
    assert_eq!(structured(&completed)["run_status"], "completed");
    driver.shutdown();
}

#[test]
fn hidden_start_run_cannot_create_shadow_before_driver_attach() {
    let fixture = DriverFixture::new("mcp-driver-shadow-run");
    let mut server = fixture.mcp_server();
    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-shadow",
            "nodes": [{ "id": "root" }]
        }),
    );
    assert_eq!(started["error"]["code"], -32602, "{started}");
    assert_eq!(started["error"]["message"], "unknown tool", "{started}");
    assert!(!fixture.run_root("run-shadow").exists());

    let mut driver = fixture.spawn("run-shadow");
    assert_eq!(
        fixture.request("run-shadow", bind_run_request("bind", "run-shadow"))["ok"],
        true
    );

    let status = call_tool(
        &mut server,
        2,
        "run_status",
        json!({
            "run_id": "run-shadow"
        }),
    );

    assert_eq!(structured(&status)["ok"], true);
    assert_eq!(structured(&status)["run_status"], "running");
    driver.shutdown();
}

#[test]
fn driver_owned_apply_flow_lock_proxies_exact_package_and_mode() {
    let fixture = DriverFixture::new("mcp-driver-apply-lock");
    let mut driver = fixture.spawn("run-apply-lock");
    assert_eq!(
        fixture.request("run-apply-lock", bind_run_request("bind", "run-apply-lock"))["ok"],
        true
    );

    let mut server = fixture.mcp_server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, updated_locked_flow());
    let applied = call_tool(
        &mut server,
        2,
        "apply_flow_lock",
        json!({
            "run_id": "run-apply-lock",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "mode": "checkpoint_restart"
        }),
    );

    assert_eq!(structured(&applied)["ok"], true);
    assert_eq!(structured(&applied)["flow_lock_id"], lock_id);
    assert_eq!(structured(&applied)["content_hash"], content_hash);
    assert_eq!(structured(&applied)["apply_mode"], "checkpoint_restart");
    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-apply-lock"
        }),
    );
    let revision = structured(&status)["context"]["flow_revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|revision| revision["content_hash"] == content_hash)
        .expect("applied revision should be owned by the driver");
    assert_eq!(revision["mode"], "checkpoint_restart");
    driver.shutdown();
}

#[test]
fn driver_read_and_run_asset_handlers_use_authoritative_state() {
    let fixture = DriverFixture::new("mcp-driver-read-handlers");
    let mut driver = fixture.spawn("run-read-handlers");
    assert_eq!(
        fixture.request(
            "run-read-handlers",
            bind_run_request("bind", "run-read-handlers")
        )["ok"],
        true
    );
    let mut server = fixture.mcp_server();

    let context = call_tool(
        &mut server,
        1,
        "get_context",
        json!({ "run_id": "run-read-handlers" }),
    );
    assert_eq!(structured(&context)["ok"], true, "{context}");
    assert_eq!(
        structured(&context)["context"]["run_id"],
        "run-read-handlers"
    );

    let validation = call_tool(
        &mut server,
        2,
        "validate_stop",
        json!({
            "run_id": "run-read-handlers",
            "activation_id": "root"
        }),
    );
    assert_eq!(structured(&validation)["ok"], false, "{validation}");
    assert_eq!(structured(&validation)["valid"], false);
    assert_eq!(
        structured(&validation)["missing"],
        json!(["artifact:brief"])
    );

    let terminal = call_tool(
        &mut server,
        3,
        "view_terminal",
        json!({ "run_id": "run-read-handlers" }),
    );
    assert_eq!(structured(&terminal)["ok"], true, "{terminal}");
    assert_eq!(structured(&terminal)["format"], "terminal");
    assert_eq!(structured(&terminal)["run_count"], 1);

    let snapshot = call_tool(
        &mut server,
        4,
        "view_snapshot",
        json!({ "run_id": "run-read-handlers" }),
    );
    assert_eq!(structured(&snapshot)["ok"], true, "{snapshot}");
    assert_eq!(
        structured(&snapshot)["snapshot"]["runs"][0]["run_id"],
        "run-read-handlers"
    );

    let hook = fixture.request(
        "run-read-handlers",
        json!({
            "id": "record-read-hook",
            "op": "record_hook_fact",
            "run_id": "run-read-handlers",
            "session_id": "session-read",
            "activation_id": "root",
            "hook": "compaction_pending",
            "source_native_id": "hook-read-1",
            "payload": { "reason": "test" }
        }),
    );
    assert_eq!(hook["ok"], true, "{hook}");
    assert!(hook["context_generation"].as_u64().is_some());
    let hook_records = fs::read_to_string(
        fixture
            .run_root("run-read-handlers")
            .join("records/events.jsonl"),
    )
    .unwrap();
    assert!(!hook_records.contains("hook-read-1"));
    assert!(hook_records.lines().any(|line| {
        serde_json::from_str::<Value>(line).is_ok_and(|event| event["kind"] == "hook.observed")
    }));
    driver.shutdown();
}

#[test]
fn driver_hook_fact_validation_preserves_live_authority_contract() {
    let fixture = DriverFixture::new("mcp-driver-hook-validation");
    let mut driver = fixture.spawn("run-hook-validation");
    assert_eq!(
        fixture.request(
            "run-hook-validation",
            bind_run_request("bind", "run-hook-validation")
        )["ok"],
        true
    );
    for (id, arguments, expected) in [
        (
            1,
            json!({
                "run_id": "run-hook-validation",
                "session_id": "",
                "hook": "compaction_pending"
            }),
            "session_id must be non-empty",
        ),
        (
            2,
            json!({
                "run_id": "run-hook-validation",
                "session_id": "host-a",
                "hook": "unknown_hook"
            }),
            "hook must be a documented hook name or namespaced extension",
        ),
        (
            3,
            json!({
                "run_id": "run-hook-validation",
                "session_id": "host-a",
                "hook": "vendor.custom_hook",
                "source_native_id": ""
            }),
            "source_native_id must be non-empty",
        ),
        (
            4,
            json!({
                "run_id": "run-hook-validation",
                "session_id": "host-a",
                "activation_id": "missing",
                "hook": "compaction_pending"
            }),
            "activation not found",
        ),
        (
            5,
            json!({
                "run_id": "run-hook-validation",
                "session_id": "host-a",
                "hook": "compaction_pending",
                "payload": "x".repeat(70000)
            }),
            "payload exceeds",
        ),
    ] {
        let mut request = arguments;
        request["id"] = json!(format!("invalid-hook-{id}"));
        request["op"] = json!("record_hook_fact");
        let response = fixture.request("run-hook-validation", request);
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(expected)),
            "{response}"
        );
    }
    driver.shutdown();
}

#[test]
fn driver_read_surfaces_share_one_authoritative_snapshot() {
    let fixture = DriverFixture::new("mcp-driver-read-parity");
    let mut driver = fixture.spawn("run-read-parity");
    assert_eq!(
        fixture.request(
            "run-read-parity",
            bind_run_request("bind", "run-read-parity")
        )["ok"],
        true
    );
    let mut server = fixture.mcp_server();

    let context = call_tool(
        &mut server,
        1,
        "get_context",
        json!({ "run_id": "run-read-parity" }),
    );
    let status = call_tool(
        &mut server,
        2,
        "run_status",
        json!({ "run_id": "run-read-parity" }),
    );
    let why = call_tool(
        &mut server,
        3,
        "run_why",
        json!({ "run_id": "run-read-parity" }),
    );
    let validation = call_tool(
        &mut server,
        4,
        "validate_stop",
        json!({ "run_id": "run-read-parity", "activation_id": "root" }),
    );
    let preview = call_tool(
        &mut server,
        5,
        "preview_flow_routes",
        json!({ "run_id": "run-read-parity" }),
    );
    let terminal = call_tool(
        &mut server,
        6,
        "view_terminal",
        json!({ "run_id": "run-read-parity" }),
    );
    let snapshot = call_tool(
        &mut server,
        7,
        "view_snapshot",
        json!({ "run_id": "run-read-parity" }),
    );

    let responses = [
        &context,
        &status,
        &why,
        &validation,
        &preview,
        &terminal,
        &snapshot,
    ];
    let expected_cursor = structured(&context)["event_cursor"].clone();
    let expected_generation = structured(&context)["context_generation"].clone();
    for response in responses {
        assert_eq!(
            structured(response)["event_cursor"],
            expected_cursor,
            "{response}"
        );
        assert_eq!(
            structured(response)["context_generation"],
            expected_generation,
            "{response}"
        );
    }
    assert_eq!(
        structured(&status)["context"],
        structured(&context)["context"]
    );
    assert_eq!(
        structured(&why)["run_status"],
        structured(&context)["context"]["run_status"]
    );
    assert_eq!(
        structured(&snapshot)["snapshot"]["runs"][0]["run_status"],
        structured(&context)["context"]["run_status"]
    );
    assert_eq!(structured(&preview)["source"], "latest_applied");
    assert_eq!(structured(&terminal)["run_count"], 1);
    driver.shutdown();
}

#[test]
fn driver_argument_preparation_preserves_node_spec_board_version_and_aliases() {
    let fixture = DriverFixture::new("mcp-driver-args");
    let mut driver = fixture.spawn("run-argument-preparation");
    assert_eq!(
        fixture.request(
            "run-argument-preparation",
            bind_run_request("bind", "run-argument-preparation")
        )["ok"],
        true
    );
    let mut server = fixture.mcp_server();

    let activated = call_tool(
        &mut server,
        1,
        "activate_node",
        json!({
            "runId": "run-argument-preparation",
            "nodeId": "manual",
            "requiredArtifacts": ["manual-output"],
            "requiredEffects": ["manual-effect"]
        }),
    );
    assert_eq!(structured(&activated)["ok"], true, "{activated}");
    assert_eq!(structured(&activated)["activation_id"], "manual");

    let validation = call_tool(
        &mut server,
        2,
        "validate_stop",
        json!({
            "runId": "run-argument-preparation",
            "activationId": "manual"
        }),
    );
    assert_eq!(structured(&validation)["ok"], false, "{validation}");
    assert_eq!(
        structured(&validation)["missing"],
        json!(["artifact:manual-output"])
    );

    let conflict = call_tool(
        &mut server,
        3,
        "patch_board",
        json!({
            "runId": "run-argument-preparation",
            "activationId": "root",
            "expectedVersion": 9,
            "patch": { "first": "one", "second": "two" }
        }),
    );
    assert_eq!(structured(&conflict)["ok"], false, "{conflict}");
    let status = call_tool(
        &mut server,
        4,
        "run_status",
        json!({ "run_id": "run-argument-preparation" }),
    );
    assert!(
        structured(&status)["context"]["board"]
            .as_object()
            .is_some_and(|board| board.is_empty())
    );

    let patched = call_tool(
        &mut server,
        5,
        "patch_board",
        json!({
            "runId": "run-argument-preparation",
            "activationId": "root",
            "expectedVersion": 0,
            "patch": { "first": "one", "second": "two" }
        }),
    );
    assert_eq!(structured(&patched)["ok"], true, "{patched}");
    assert_eq!(
        structured(&patched)["board_version"],
        structured(&patched)["event_cursor"]
    );
    let status = call_tool(
        &mut server,
        6,
        "run_status",
        json!({ "run_id": "run-argument-preparation" }),
    );
    let versions = &structured(&status)["context"]["board_versions"];
    assert!(versions["first"].as_u64().unwrap() < versions["second"].as_u64().unwrap());
    assert_eq!(versions["second"], structured(&patched)["board_version"]);

    let stale_second_key = call_tool(
        &mut server,
        7,
        "patch_board",
        json!({
            "runId": "run-argument-preparation",
            "activationId": "root",
            "expectedVersion": 0,
            "patch": { "aaa-new": "new", "second": "stale" }
        }),
    );
    assert_eq!(
        structured(&stale_second_key)["ok"],
        false,
        "{stale_second_key}"
    );
    let status = call_tool(
        &mut server,
        8,
        "run_status",
        json!({ "run_id": "run-argument-preparation" }),
    );
    assert!(
        structured(&status)["context"]["board"]
            .get("aaa-new")
            .is_none(),
        "{status}"
    );
    driver.shutdown();
}

#[test]
fn driver_owned_preview_flow_routes_uses_exact_package_without_mutation() {
    let fixture = DriverFixture::new("mcp-driver-preview-gate");
    let mut driver = fixture.spawn("run-preview-gate");
    assert_eq!(
        fixture.request(
            "run-preview-gate",
            bind_run_request("bind", "run-preview-gate")
        )["ok"],
        true
    );

    let mut server = fixture.mcp_server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, updated_locked_flow());
    let before = fixture.request(
        "run-preview-gate",
        json!({
            "id": "before-preview",
            "op": "status",
            "run_id": "run-preview-gate"
        }),
    );
    let preview = call_tool(
        &mut server,
        2,
        "preview_flow_routes",
        json!({
            "run_id": "run-preview-gate",
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );

    assert_eq!(structured(&preview)["ok"], true, "{preview}");
    assert_eq!(structured(&preview)["flow_lock_id"], lock_id);
    assert_eq!(structured(&preview)["content_hash"], content_hash);
    assert_eq!(structured(&preview)["source"], "explicit");
    assert!(structured(&preview)["routes"].is_array());
    let after = fixture.request(
        "run-preview-gate",
        json!({
            "id": "after-preview",
            "op": "status",
            "run_id": "run-preview-gate"
        }),
    );
    assert_eq!(after["event_cursor"], before["event_cursor"]);
    assert_eq!(after["context_generation"], before["context_generation"]);
    driver.shutdown();
}

#[test]
fn mcp_existing_run_does_not_fall_back_to_process_local_state_when_driver_is_absent() {
    let fixture = DriverFixture::new("mcp-driver-no-fallback");
    let mut driver = fixture.spawn("run-stale");
    assert_eq!(
        fixture.request("run-stale", bind_run_request("bind", "run-stale"))["ok"],
        true
    );
    driver.crash();

    let mut server = fixture.mcp_server();
    let status = call_tool(
        &mut server,
        1,
        "run_status",
        json!({
            "run_id": "run-stale"
        }),
    );

    assert_eq!(structured(&status)["ok"], false);
    assert_eq!(structured(&status)["error"]["code"], "driver_unavailable");
    assert_eq!(structured(&status)["recovery"]["action"], "resume_run");
    assert_eq!(structured(&status)["recovery"]["automatic_restart"], true);
    assert_ne!(structured(&status)["error"], "run not found");
}

#[test]
fn missing_driver_run_never_falls_back_for_read_mutation_or_message_routes() {
    let fixture = DriverFixture::new("mcp-driver-no-local-authority");
    let mut server = fixture.mcp_server();

    for (id, tool, arguments) in [
        (1, "run_status", json!({ "run_id": "run-missing" })),
        (2, "pause_run", json!({ "run_id": "run-missing" })),
        (
            3,
            "send_message",
            json!({
                "run_id": "run-missing",
                "activation_id": "root",
                "message_id": "message-1",
                "text": "hello"
            }),
        ),
    ] {
        let response = call_tool(&mut server, id, tool, arguments);
        assert_eq!(structured(&response)["ok"], false, "{response}");
        assert_eq!(
            structured(&response)["error"]["code"],
            "driver_authority_required",
            "{response}"
        );
    }

    let hook = call_tool(
        &mut server,
        4,
        "record_hook_fact",
        json!({
            "run_id": "run-missing",
            "session_id": "native-session-missing",
            "hook": "compaction_pending",
            "source_native_id": "native-hook-missing",
            "payload": {"reason": "test"}
        }),
    );
    assert_eq!(hook["error"]["code"], -32602, "{hook}");
    assert_eq!(hook["error"]["message"], "unknown tool", "{hook}");

    assert!(!fixture.run_root("run-missing").exists());
}

struct DriverFixture {
    _guard: MutexGuard<'static, ()>,
    root: PathBuf,
    runtime_root: PathBuf,
    token: &'static str,
}

impl DriverFixture {
    fn new(name: &str) -> Self {
        let guard = test_guard();
        let root = std::env::temp_dir()
            .join("humanize-plugin-mcp-driver-tests")
            .join(format!("{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(root.join("runtime")).unwrap();
        fs::create_dir_all(root.join("runs")).unwrap();
        Self {
            _guard: guard,
            root,
            runtime_root: test_state_root().join("runtime"),
            token: "test-token",
        }
    }

    fn spawn(&self, run_id: &str) -> DriverProcess {
        let child = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-driver"))
            .arg("--run-id")
            .arg(run_id)
            .arg("--runs-root")
            .arg(self.root.join("runs"))
            .arg("--runtime-root")
            .arg(&self.runtime_root)
            .arg("--review-root")
            .arg(self.root.join("runs/reviews"))
            .arg("--auth-token")
            .arg(self.token)
            .env("HUMANIZE_STATE_ROOT", test_state_root())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_for_socket(&self.socket_path(run_id));
        DriverProcess { child }
    }

    fn mcp_server(&self) -> McpServer<RecordingRunner> {
        McpServer::with_tmux_runner_and_run_asset_store(
            RecordingRunner::default(),
            RunAssetStore::new(RunAssetSink::HumanizeRunsDir(self.root.join("runs"))),
        )
    }

    fn socket_path(&self, run_id: &str) -> PathBuf {
        socket_path_for_run_root(&self.runtime_root, &self.run_root(run_id))
            .expect("driver socket path should resolve")
    }

    fn run_root(&self, run_id: &str) -> PathBuf {
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(self.root.join("runs")))
            .run_root(run_id)
            .unwrap()
    }

    fn request(&self, run_id: &str, mut request: Value) -> Value {
        if request.get("flow_lock").is_some() && request.get("review_id").is_none() {
            let lock = serde_json::from_value::<FlowLock>(request["flow_lock"].clone()).unwrap();
            let store = ReviewStore::new(self.root.join("runs/reviews"));
            let review = store
                .prepare(
                    &lock,
                    &json!({"title":"MCP driver proxy fixture"}),
                    "<title>MCP driver proxy fixture</title>\n",
                )
                .unwrap();
            let review = store
                .decide(review.review_id(), ReviewDecision::Approved, None)
                .unwrap();
            request["review_id"] = json!(review.review_id());
        }
        request["token"] = json!(self.token);
        let mut stream = UnixStream::connect(self.socket_path(run_id)).unwrap();
        stream
            .write_all((request.to_string() + "\n").as_bytes())
            .unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    }
}

fn test_guard() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn test_state_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir()
            .join("humanize-plugin-mcp-driver-tests")
            .join(format!("state-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        unsafe {
            std::env::set_var("HUMANIZE_STATE_ROOT", &root);
        }
        root
    })
}

struct DriverProcess {
    child: Child,
}

impl DriverProcess {
    fn shutdown(&mut self) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, "quit");
            let _ = stdin.flush();
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn crash(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

fn wait_for_socket(path: &Path) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("driver socket was not ready at {}", path.display());
}

fn bind_run_request(id: &str, run_id: &str) -> Value {
    json!({
        "id": id,
        "op": "bind_run",
        "run_id": run_id,
        "flow_lock": locked_flow_package(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    })
}

fn locked_flow_package() -> Value {
    let draft = FlowDraft {
        nodes: vec![FlowNode {
            id: "root".into(),
            contract_id: Some("contract.root".into()),
            ..FlowNode::default()
        }],
        contracts: vec![FlowContract {
            id: "contract.root".into(),
            completion: Some(ContractCompletion::AllArtifacts),
            artifacts: vec![ContractArtifact {
                id: "brief".into(),
                schema_resource_id: Some("schema.root.brief".into()),
            }],
        }],
        routes: Vec::new(),
        resources: vec![
            FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: "inline:Runtime driver locked flow.".into(),
            },
            FlowResource {
                id: "schema.root.brief".into(),
                kind: ResourceKind::Schema,
                source: "inline:brief".into(),
            },
        ],
        imports: Vec::new(),
        policies: FlowPolicies::default(),
        extensions: Vec::new(),
    };
    let lock = flow::flow_lock(&draft, FlowCheckMode::Core).unwrap();
    serde_json::to_value(lock).unwrap()
}

fn updated_locked_flow() -> Value {
    json!({
        "nodes": [
            {
                "id": "root",
                "contract_id": "contract.root"
            }
        ],
        "contracts": [
            {
                "id": "contract.root",
                "completion": "all_artifacts",
                "artifacts": [
                    {
                        "id": "brief",
                        "schema_resource_id": "schema.root.brief"
                    }
                ]
            }
        ],
        "resources": [
            {
                "path": "README.md",
                "kind": "readme",
                "content": "Updated runtime driver locked flow."
            },
            {
                "path": "schema.root.brief",
                "kind": "schema",
                "content": "brief"
            }
        ]
    })
}
