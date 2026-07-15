#[path = "support/driver_flows.rs"]
#[allow(dead_code)]
mod driver_flows;
#[path = "support/driver_tmux.rs"]
mod driver_tmux_support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::driver::socket_path_for_run_root;
use humanize_plugin::run_assets::{RunAssetManifest, RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use driver_flows::{
    approved_review_id, board_routed_agent_flow, quiescent_locked_flow, reviewed_lock_package,
    routed_locked_flow,
};
use driver_tmux_support::ControlledTmuxFixture;

#[test]
fn driver_bind_rejects_run_mode_and_initial_activation_limit_conflicts() {
    let fixture = DriverFixture::new("generation-bind-conflict");
    let mut driver = fixture.spawn("run-bind-config", &[]);
    let package = routed_locked_flow();

    let bound = fixture.request(json!({
        "id": "bind",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-bind-config",
        "flow_lock": package,
        "run_mode": "continuous",
        "activation_limit": 5
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    assert_eq!(bound["run_mode"], "continuous");
    assert_eq!(bound["initial_activation_limit"], 5);

    for (run_mode, activation_limit) in [("manual", 5), ("continuous", 6)] {
        let conflict = fixture.request(json!({
            "id": "rebind",
            "token": fixture.token,
            "op": "bind_run",
            "run_id": "run-bind-config",
            "flow_lock": package,
            "run_mode": run_mode,
            "activation_limit": activation_limit
        }));
        assert_eq!(conflict["ok"], false, "{conflict}");
        assert_eq!(conflict["error"]["code"], "run_binding_conflict");
    }
    driver.shutdown();
}

#[test]
fn pane_replacement_increments_allocation_generation_and_rejects_old_evidence() {
    let fixture = DriverFixture::new("generation-pane-replacement");
    let fake_tmux = fixture.fake_tmux_reusing_pane_id();
    fs::write(fixture.root.join("reuse-pane-id"), "reuse").unwrap();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let envs = [
        ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
        ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
    ];
    let mut driver = fixture.spawn("run-allocation", &envs);

    let bound = fixture.request(json!({
        "id": "bind",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-allocation",
        "flow_lock": reviewed_lock_package(),
        "run_mode": "continuous",
        "activation_limit": 4,
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        }
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    assert_eq!(bound["tmux"]["panes"][0]["allocation_generation"], 0);
    assert_eq!(
        bound["tmux"]["actuation"]["warnings"][0]["status"],
        "readiness_pending"
    );
    assert!(
        fs::read_to_string(fixture.root.join("tmux.log"))
            .unwrap()
            .contains("Deliver required outputs through Humanize")
    );
    driver.shutdown();

    let mut restarted = fixture.spawn("run-allocation", &envs);
    let pending = fixture.request(json!({
        "id": "resume-without-resolution",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-allocation"
    }));
    assert_eq!(pending["ok"], true, "{pending}");
    assert_eq!(pending["run_status"], "paused");
    assert_eq!(pending["tmux_allocations"], json!([]));
    assert!(
        pending["actuation"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["role"] == "node_prompt")
    );

    let status = fixture.request(json!({
        "id": "status",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-allocation"
    }));
    let barriers = status["context"]["ambiguous_deliveries"]
        .as_array()
        .unwrap();
    if let Some(agent_launch_barrier) = barriers
        .iter()
        .find(|delivery| delivery["role"] == "agent_launch")
    {
        let resolved_launch = fixture.request(json!({
            "id": "resolve-old-launch",
            "token": fixture.token,
            "op": "resume",
            "run_id": "run-allocation",
            "delivery_resolution": {
                "started_event_sequence": agent_launch_barrier["started_event_sequence"],
                "outcome": "submitted",
                "evidence": "old pane started the prior agent process before release"
            }
        }));
        assert_eq!(resolved_launch["ok"], true, "{resolved_launch}");
        assert_eq!(resolved_launch["run_status"], "paused");
    }
    let old_prompt_barrier = barriers
        .iter()
        .find(|delivery| delivery["role"] == "node_prompt")
        .unwrap();
    let _ = fs::remove_file(fixture.root.join("ready.once"));
    let resumed = fixture.request(json!({
        "id": "resolve-old-prompt",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-allocation",
        "delivery_resolution": {
            "started_event_sequence": old_prompt_barrier["started_event_sequence"],
            "outcome": "not_submitted",
            "evidence": "old pane did not retain the prompt before release"
        }
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(resumed["run_status"], "running");
    assert_eq!(resumed["tmux_allocations"][0]["pane_id"], "%8");
    assert_eq!(resumed["tmux_allocations"][0]["allocation_generation"], 1);
    assert_eq!(
        resumed["actuation"]["warnings"][0]["status"],
        "readiness_pending"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("tmux.log"))
            .unwrap()
            .matches("Deliver required outputs through Humanize")
            .count(),
        2
    );

    let private_manifest: Value = serde_json::from_slice(
        &fs::read(
            fixture
                .private_driver_dir("run-allocation")
                .join("run-assets.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        private_manifest["activations"]["root"]["allocation_generation"],
        1
    );
    assert!(
        private_manifest["activations"]["root"]["relative_paths"]["transcript_pipe"]
            .as_str()
            .unwrap()
            .contains("allocation-1")
    );
    assert!(
        private_manifest["activations"]["root"]["readiness_nonce"]
            .as_str()
            .is_some_and(|nonce| !nonce.is_empty())
    );
    let run_root = fixture.run_root("run-allocation");
    let public_manifest: Value =
        serde_json::from_slice(&fs::read(run_root.join("manifest.json")).unwrap()).unwrap();
    assert!(!public_manifest.to_string().contains("readiness_nonce"));
    let binding = fixture.participant_binding("run-allocation", "root", 1);
    let readiness_nonce = binding["readiness_nonce"].as_str().unwrap();
    let public_status = fixture.request(json!({
        "id": "status-with-private-readiness-identity",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-allocation"
    }));
    assert!(!public_status.to_string().contains(readiness_nonce));
    assert!(!public_status.to_string().contains("readiness_nonce"));
    assert!(
        !fs::read_to_string(fixture.root.join("tmux.log"))
            .unwrap()
            .contains(readiness_nonce)
    );
    assert!(
        fs::read_dir(fixture.private_run_root("run-allocation").join("bindings"))
            .unwrap()
            .flatten()
            .any(|entry| fs::read_to_string(entry.path())
                .is_ok_and(|binding| binding.contains(readiness_nonce)))
    );

    assert!(!run_root.join("machine-inputs.jsonl").exists());
    let ledger = fs::read_to_string(
        fixture
            .private_driver_dir("run-allocation")
            .join("machine-inputs.jsonl"),
    )
    .unwrap();
    let records = ledger
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(records.iter().any(|record| {
        record["allocation_generation"] == 0
            && record["normalized_text"]
                .as_str()
                .is_some_and(|text| text.starts_with("Create the brief."))
            && record["status"] == "submitted"
    }));
    assert!(records.iter().any(|record| {
        record["allocation_generation"] == 1
            && record["normalized_text"] == "participant-agent-launch"
            && record["status"] == "submitted"
    }));
    assert!(records.iter().any(|record| {
        record["allocation_generation"] == 1
            && record["normalized_text"]
                .as_str()
                .is_some_and(|text| text.starts_with("Create the brief."))
            && record["status"] == "submitted"
    }));
    let tmux_log = fs::read_to_string(fixture.root.join("tmux.log")).unwrap();
    assert_eq!(tmux_log.matches("Create the brief.").count(), 2);
    restarted.shutdown();
}

#[test]
fn pane_cleanup_recovers_after_kill_before_receipt_and_reuses_target() {
    let fixture = DriverFixture::new("cleanup-recover");
    let fake_tmux = fixture.fake_tmux_reusing_pane_id();
    fs::write(fixture.root.join("reuse-pane-id"), "reuse").unwrap();
    let crash_marker = fixture.root.join("crash-after-pane-kill");
    fs::write(&crash_marker, "crash").unwrap();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let crash_marker_value = crash_marker.to_string_lossy().to_string();
    let mut driver = fixture.spawn(
        "run-cleanup-recovery",
        &[
            ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
            (
                "HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_IF_EXISTS",
                crash_marker_value.as_str(),
            ),
            (
                "HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_KIND",
                "pane_killed",
            ),
        ],
    );
    let bound = fixture.request(json!({
        "id": "bind",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-cleanup-recovery",
        "flow_lock": reviewed_lock_package(),
        "run_mode": "continuous",
        "activation_limit": 4,
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        }
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    assert_eq!(bound["tmux"]["panes"][0]["pane_id"], "%8");
    assert_eq!(bound["tmux"]["panes"][0]["allocation_generation"], 0);

    let status = driver.shutdown_status();
    assert_eq!(status.code(), Some(86), "{status}");
    assert!(!crash_marker.exists());
    let events_path = fixture
        .private_driver_dir("run-cleanup-recovery")
        .join("driver-events.jsonl");
    let events_before_restart = fs::read_to_string(&events_path).unwrap();
    assert_eq!(
        events_before_restart
            .matches("tmux_pane_cleanup_intent")
            .count(),
        1
    );
    assert!(!events_before_restart.contains("tmux_pane_cleanup_receipt"));
    fs::write(fixture.root.join("pane-8.alive"), "reused target").unwrap();

    let mut restarted = fixture.spawn(
        "run-cleanup-recovery",
        &[
            ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
        ],
    );
    let recovered = fixture.request(json!({
        "id": "status-after-recovery",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-cleanup-recovery"
    }));
    assert_eq!(recovered["ok"], true, "{recovered}");
    let manifest: RunAssetManifest = serde_json::from_slice(
        &fs::read(
            fixture
                .private_driver_dir("run-cleanup-recovery")
                .join("run-assets.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let root = manifest.activations.get("root").unwrap();
    assert!(root.capture_complete);
    assert_eq!(root.resource_cleanup_status, "complete");
    assert!(
        !fixture
            .private_run_root("run-cleanup-recovery")
            .join("bindings")
            .exists()
    );

    let events_after_restart = fs::read_to_string(&events_path).unwrap();
    assert_eq!(
        events_after_restart
            .matches("tmux_pane_cleanup_intent")
            .count(),
        1
    );
    assert_eq!(
        events_after_restart
            .matches("tmux_pane_cleanup_receipt")
            .count(),
        1
    );
    assert_eq!(
        events_after_restart.matches("tmux_panes_released").count(),
        1
    );
    let tmux_log = fs::read_to_string(fixture.root.join("tmux.log")).unwrap();
    assert_eq!(tmux_log.matches("kill-pane -t host-a:%7.%8").count(), 1);
    assert!(fixture.root.join("pane-8.alive").exists());
    let tmux_records = fs::read_to_string(
        fixture
            .run_root("run-cleanup-recovery")
            .join("records/events.jsonl"),
    )
    .unwrap();
    let tmux_records = tmux_records
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        tmux_records
            .iter()
            .filter(|event| {
                event["kind"] == "activation.completed"
                    && event["data"]["payload"]["state"] == "completed"
                    && event["data"]["payload"]["allocation_generation"] == 0
            })
            .count(),
        1
    );
    assert_eq!(
        tmux_records
            .iter()
            .filter(|event| {
                event["kind"] == "activation.completed"
                    && event["data"]["payload"]["state"] == "closed"
                    && event["data"]["payload"]["allocation_generation"] == 0
            })
            .count(),
        1
    );

    let barriers = recovered["context"]["ambiguous_deliveries"]
        .as_array()
        .unwrap();
    if let Some(agent_launch) = barriers
        .iter()
        .find(|delivery| delivery["role"] == "agent_launch")
    {
        let resolved = fixture.request(json!({
            "id": "resolve-agent-launch",
            "token": fixture.token,
            "op": "resume",
            "run_id": "run-cleanup-recovery",
            "delivery_resolution": {
                "started_event_sequence": agent_launch["started_event_sequence"],
                "outcome": "submitted",
                "evidence": "the prior allocation submitted the agent launch before cleanup"
            }
        }));
        assert_eq!(resolved["ok"], true, "{resolved}");
    }
    let prompt = barriers
        .iter()
        .find(|delivery| delivery["role"] == "node_prompt")
        .unwrap();
    let resumed = fixture.request(json!({
        "id": "resolve-node-prompt",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-cleanup-recovery",
        "delivery_resolution": {
            "started_event_sequence": prompt["started_event_sequence"],
            "outcome": "not_submitted",
            "evidence": "the cleaned allocation cannot retain the prior node prompt"
        }
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(resumed["tmux_allocations"][0]["pane_id"], "%8");
    assert_eq!(resumed["tmux_allocations"][0]["allocation_generation"], 1);
    restarted.shutdown();
}

#[test]
fn committed_route_trigger_replays_after_pane_allocation_crash_without_duplication() {
    let fixture = DriverFixture::new("generation-route-crash");
    let fake_tmux = fixture.fake_tmux_reusing_pane_id();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let envs = [("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str())];
    let mut driver = fixture.spawn("run-route-crash", &envs);

    let bound = fixture.request(json!({
        "id": "bind",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-route-crash",
        "flow_lock": routed_locked_flow(),
        "run_mode": "continuous",
        "activation_limit": 4,
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "noop-agent"
        }
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    fs::write(fixture.root.join("fail-next-split"), "fail").unwrap();

    let failed = fixture.request(json!({
        "id": "deliver",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-route-crash",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": "ready"
    }));
    assert_eq!(failed["ok"], false, "{failed}");
    driver.crash();

    let mut restarted = fixture.spawn("run-route-crash", &envs);
    let resumed = fixture.request(json!({
        "id": "resume",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-route-crash"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(resumed["tmux_allocations"][0]["activation_id"], "follow");

    let events = fs::read_to_string(
        fixture
            .private_driver_dir("run-route-crash")
            .join("events.jsonl"),
    )
    .unwrap();
    let follow_activations = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .flat_map(|batch| batch["events"].as_array().cloned().unwrap_or_default())
        .filter(|event| {
            event["payload"]["type"] == "node_activated" && event["payload"]["node_id"] == "follow"
        })
        .collect::<Vec<_>>();
    assert_eq!(follow_activations.len(), 1);
    assert_eq!(follow_activations[0]["payload"]["activation_generation"], 0);
    assert!(follow_activations[0]["payload"]["trigger"].is_object());
    restarted.shutdown();
}

#[test]
fn paused_fact_mutation_commits_without_allocating_or_actuating() {
    let fixture = DriverFixture::new("paused-reconcile");
    let fake_tmux = fixture.fake_tmux_reusing_pane_id();
    fs::write(
        fixture.root.join("ready.once"),
        "suppress automatic readiness",
    )
    .unwrap();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let envs = [
        ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
        ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
    ];
    let mut driver = fixture.spawn("run-paused-reconciliation", &envs);

    let bound = fixture.request(json!({
        "id": "bind",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-paused-reconciliation",
        "flow_lock": reviewed_lock_package(),
        "run_mode": "continuous",
        "activation_limit": 4,
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        }
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    assert_eq!(
        bound["tmux"]["actuation"]["sent"].as_array().unwrap().len(),
        0
    );
    let root_pane = bound["tmux"]["panes"][0]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let binding = fixture.participant_binding("run-paused-reconciliation", "root", 0);
    let readiness_nonce = binding["readiness_nonce"].as_str().unwrap();

    let paused = fixture.request(json!({
        "id": "pause",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-paused-reconciliation"
    }));
    assert_eq!(paused["ok"], true, "{paused}");
    assert_eq!(paused["run_status"], "paused");

    let hook = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .env("HUMANIZE_RUNS_DIR", fixture.root.join("runs"))
        .env("TMUX_PANE", root_pane)
        .env("HUMANIZE_READY_RUN_ID", "run-paused-reconciliation")
        .env("HUMANIZE_READY_ACTIVATION_ID", "root")
        .env("HUMANIZE_READY_ALLOCATION_GENERATION", "0")
        .env("HUMANIZE_READY_NONCE", readiness_nonce)
        .arg("--agent-ready-hook")
        .arg("--source")
        .arg("codex_session_start")
        .output()
        .unwrap();
    assert!(hook.status.success());
    let before_log = fs::read_to_string(fixture.root.join("tmux.log")).unwrap();

    let delivered = fixture.request(json!({
        "id": "deliver",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-paused-reconciliation",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": "ready"
    }));
    assert_eq!(delivered["ok"], true, "{delivered}");
    assert_eq!(delivered["tmux_allocations"], json!([]));
    assert_eq!(delivered["actuation"]["sent"], json!([]));
    assert_eq!(delivered["run_status"], "paused");

    let after_log = fs::read_to_string(fixture.root.join("tmux.log")).unwrap();
    assert_eq!(after_log, before_log);
    let status = fixture.request(json!({
        "id": "status",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-paused-reconciliation"
    }));
    assert!(status["context"]["activations"].get("follow").is_none());
    driver.shutdown();
}

#[test]
fn driver_explicit_scheduling_rejects_quiescent_without_mutation() {
    let fixture = DriverFixture::new("quiescent-explicit-scheduling");
    let mut driver = fixture.spawn("run-quiescent-explicit", &[]);
    let bound = fixture.request(json!({
        "id": "bind",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-quiescent-explicit",
        "flow_lock": quiescent_locked_flow(),
        "run_mode": "continuous",
        "activation_limit": 8
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    let delivered = fixture.request(json!({
        "id": "deliver-items",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-quiescent-explicit",
        "activation_id": "root",
        "artifact_key": "items",
        "payload": "alpha\nbeta"
    }));
    assert_eq!(delivered["ok"], true, "{delivered}");
    let stopped = fixture.request(json!({
        "id": "stop-root",
        "token": fixture.token,
        "op": "observe_stop",
        "run_id": "run-quiescent-explicit",
        "activation_id": "root",
        "reason": "done"
    }));
    assert_eq!(stopped["run_status"], "quiescent", "{stopped}");

    let before = fixture.request(json!({
        "id": "status-before",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-quiescent-explicit"
    }));
    for request in [
        json!({
            "id": "activate",
            "token": fixture.token,
            "op": "activate",
            "run_id": "run-quiescent-explicit",
            "node_id": "manual"
        }),
        json!({
            "id": "fanout",
            "token": fixture.token,
            "op": "fanout",
            "run_id": "run-quiescent-explicit",
            "node_id": "batch",
            "artifact_key": "items",
            "for_each": "items"
        }),
    ] {
        let rejected = fixture.request(request);
        assert_eq!(rejected["ok"], false, "{rejected}");
        assert_eq!(rejected["error"]["code"], "runtime_error", "{rejected}");
        let after = fixture.request(json!({
            "id": "status-after",
            "token": fixture.token,
            "op": "status",
            "run_id": "run-quiescent-explicit"
        }));
        assert_eq!(after["run_status"], "quiescent", "{after}");
        assert_eq!(after["event_cursor"], before["event_cursor"], "{after}");
        assert_eq!(
            after["context_generation"], before["context_generation"],
            "{after}"
        );
        assert_eq!(
            after["context"]["activations"], before["context"]["activations"],
            "{after}"
        );
    }
    driver.shutdown();
}

#[test]
fn board_and_effect_mutations_use_post_commit_reconciliation() {
    let fixture = DriverFixture::new("post-commit-reconcile");
    let fake_tmux = fixture.fake_tmux_reusing_pane_id();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let envs = [("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str())];
    let mut driver = fixture.spawn("run-post-commit", &envs);

    let bound = fixture.request(json!({
        "id": "bind",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-post-commit",
        "flow_lock": board_routed_agent_flow(),
        "run_mode": "continuous",
        "activation_limit": 4,
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        }
    }));
    assert_eq!(bound["ok"], true, "{bound}");

    let patched = fixture.request(json!({
        "id": "patch",
        "token": fixture.token,
        "op": "patch_board",
        "run_id": "run-post-commit",
        "activation_id": "root",
        "patch": { "ready": "true" }
    }));
    assert_eq!(patched["ok"], true, "{patched}");
    assert_eq!(patched["tmux_allocations"][0]["activation_id"], "follow");
    assert_eq!(
        patched["actuation"]["warnings"][0]["status"],
        "readiness_pending"
    );
    assert!(
        fs::read_to_string(fixture.root.join("tmux.log"))
            .unwrap()
            .contains("Handle the board fact.")
    );

    let before = fs::read_to_string(fixture.root.join("tmux.log")).unwrap();
    let effect = fixture.request(json!({
        "id": "effect",
        "token": fixture.token,
        "op": "record_effect",
        "run_id": "run-post-commit",
        "activation_id": "root",
        "effect_key": "audit",
        "payload": "recorded"
    }));
    assert_eq!(effect["ok"], true, "{effect}");
    assert_eq!(effect["tmux_allocations"], json!([]));
    assert_eq!(effect["actuation"]["sent"], json!([]));
    assert_eq!(
        fs::read_to_string(fixture.root.join("tmux.log")).unwrap(),
        before
    );
    driver.shutdown();
}

struct DriverFixture {
    root: PathBuf,
    token: &'static str,
    tmux_control: ControlledTmuxFixture,
}

impl DriverFixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .join("humanize-plugin-generation-tests")
            .join(format!("{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(root.join("runtime")).unwrap();
        fs::create_dir_all(root.join("runs")).unwrap();
        let tmux_control = ControlledTmuxFixture::new(&root);
        Self {
            root,
            token: "test-token",
            tmux_control,
        }
    }

    fn spawn(&self, run_id: &str, envs: &[(&str, &str)]) -> DriverProcess {
        let mut command = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-driver"));
        command
            .arg("--run-id")
            .arg(run_id)
            .arg("--runs-root")
            .arg(self.root.join("runs"))
            .arg("--runtime-root")
            .arg(self.root.join("runtime"))
            .arg("--review-root")
            .arg(self.root.join("reviews"))
            .arg("--auth-token")
            .arg(self.token)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .env("HUMANIZE_STATE_ROOT", &self.root);
        for (key, value) in envs {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        wait_for_socket(&mut child, &self.socket_path(run_id));
        DriverProcess { child }
    }

    fn request(&self, request: Value) -> Value {
        let request = self.with_review_id(request);
        let run_id = request["run_id"].as_str().unwrap();
        let mut stream = UnixStream::connect(self.socket_path(run_id)).unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        assert!(self.tmux_control.wait_for_hooks());
        serde_json::from_str(&response).unwrap()
    }

    fn with_review_id(&self, mut request: Value) -> Value {
        if request.get("flow_lock").is_some()
            && request.get("review_id").is_none()
            && request.get("reviewId").is_none()
        {
            let review_id = approved_review_id(&self.root.join("reviews"), &request["flow_lock"]);
            request["review_id"] = Value::String(review_id);
        }
        request
    }

    fn socket_path(&self, run_id: &str) -> PathBuf {
        socket_path_for_run_root(&self.root.join("runtime"), &self.run_root(run_id))
    }

    fn run_root(&self, run_id: &str) -> PathBuf {
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(self.root.join("runs")))
            .run_root(run_id)
            .unwrap()
    }

    fn private_run_root(&self, run_id: &str) -> PathBuf {
        let run_root = self.run_root(run_id);
        let identity = std::path::absolute(&run_root)
            .unwrap_or(run_root)
            .to_string_lossy()
            .into_owned();
        self.root
            .join("runtime")
            .join(format!("r{:016x}", stable_hash(&identity)))
    }

    fn private_driver_dir(&self, run_id: &str) -> PathBuf {
        self.private_run_root(run_id).join("driver")
    }

    fn participant_binding(
        &self,
        run_id: &str,
        activation_id: &str,
        allocation_generation: u64,
    ) -> Value {
        fs::read_to_string(self.private_driver_dir(run_id).join("driver-events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| {
                event["kind"] == "participant_started"
                    && event["payload"]["binding"]["activation_id"] == activation_id
                    && event["payload"]["binding"]["allocation_generation"] == allocation_generation
            })
            .map(|event| event["payload"]["binding"].clone())
            .unwrap()
    }

    fn fake_tmux_reusing_pane_id(&self) -> PathBuf {
        let path = self.root.join("fake-tmux-reused-pane");
        let script = format!(
            r#"#!/bin/sh
root='{}'
printf '%s\n' "$*" >> "$root/tmux.log"
last=''
target=''
previous=''
for arg in "$@"; do
  if test "$previous" = '-t'; then target="$arg"; fi
  previous="$arg"
  last="$arg"
done
load_ready_environment() {{
  eval "set -- $last"
  if test "$1" = 'env'; then shift; fi
  while test "$#" -gt 0; do
    case "$1" in
      HUMANIZE_READY_RUN_ID=*|HUMANIZE_READY_ACTIVATION_ID=*|HUMANIZE_READY_ALLOCATION_GENERATION=*|HUMANIZE_READY_NONCE=*|HUMANIZE_PARTICIPANT_RUN_ID=*|HUMANIZE_PARTICIPANT_ACTIVATION_ID=*|HUMANIZE_PARTICIPANT_HANDLE=*|HUMANIZE_PARTICIPANT_CREDENTIAL=*|HUMANIZE_PARTICIPANT_BINDING_FILE=*)
        export "$1"
        shift
        ;;
      *) break ;;
    esac
  done
}}
mark_pane_alive() {{
  pane="$1"
  : > "$root/pane-${{pane#%}}.alive"
}}
case "$1" in
  has-session)
    exit 1
    ;;
  new-session)
    mark_pane_alive '%8'
    printf '%s\t%s\n' '%7' '%8'
    ;;
  split-window)
    if test -f "$root/fail-next-split"; then
      rm -f "$root/fail-next-split"
      exit 42
    fi
    if test -f "$root/reuse-pane-id"; then
      pane='%8'
    else
      pane='%9'
    fi
    mark_pane_alive "$pane"
    printf '%s\n' "$pane"
    ;;
  display-message)
    pane="${{target##*.}}"
    test -f "$root/pane-${{pane#%}}.alive" || exit 1
    printf '%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' "$pane"
    ;;
  pipe-pane)
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" start "$root" "${{pane#%}}" "$last"
    ;;
  capture-pane)
    printf 'final capture for %s\n' "$target"
    ;;
  send-keys)
    case "$*" in
      *humanize-test-agent*)
        if test ! -f "$root/ready.once"; then
          : > "$root/ready.once"
          load_ready_environment
          pane="${{target##*.}}"
          pending="$root/hook-helper-${{pane#%}}-$$.pending"
          done="${{pending%.pending}}.done"
          : > "$pending"
          (
            printf '{{"hook_event_name":"SessionStart","session_id":"fake-native-%s"}}\n' "${{pane#%}}" |
              HUMANIZE_RUNS_DIR="$root/runs" TMUX_PANE="$pane" '{}' --agent-ready-hook --source codex_session_start
            mv "$pending" "$done"
          ) </dev/null >/dev/null 2>/dev/null &
        fi
        ;;
    esac
    ;;
  kill-pane)
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" stop "$root" "${{pane#%}}"
    rm -f "$root/pane-${{pane#%}}.alive"
    ;;
esac
exit 0
"#,
            self.root.display(),
            env!("CARGO_BIN_EXE_humanize-plugin-mcp")
        );
        fs::write(&path, script).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

struct DriverProcess {
    child: Child,
}

impl DriverProcess {
    fn shutdown_status(&mut self) -> std::process::ExitStatus {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, "quit");
            let _ = stdin.flush();
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(3) {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        panic!("driver did not shut down cleanly");
    }

    fn shutdown(&mut self) {
        let _ = self.shutdown_status();
    }

    fn crash(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

impl Drop for DriverProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn wait_for_socket(child: &mut Child, path: &Path) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(stream) = child.stderr.as_mut() {
                let _ = std::io::Read::read_to_string(stream, &mut stderr);
            }
            panic!("driver exited before socket was ready: {status}; stderr={stderr}");
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("driver socket was not ready at {}", path.display());
}
