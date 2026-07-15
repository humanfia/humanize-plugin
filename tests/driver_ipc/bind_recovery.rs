use std::fs;

use humanize_plugin::driver::load_driver_recovery_state;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::json;

use super::driver_flows::{reviewed_lock_package, routed_locked_flow};
use super::support::{DriverFixture, expected_initial_activation_ids};

#[test]
fn driver_bind_run_matches_real_runtime_initial_activation_semantics() {
    let fixture = DriverFixture::new("driver-real-runtime-initial");
    let mut driver = fixture.spawn("run-runtime-initial");

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-runtime-initial",
        "flow_lock": routed_locked_flow(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));

    assert_eq!(bound["ok"], true);
    assert_eq!(
        bound["activation_ids"],
        json!(expected_initial_activation_ids())
    );
    assert!(
        bound["activation_ids"]
            .as_array()
            .unwrap()
            .contains(&json!("root"))
    );
    assert!(
        !bound["activation_ids"]
            .as_array()
            .unwrap()
            .contains(&json!("follow"))
    );
    driver.shutdown();
}

#[test]
fn committed_runtime_and_immutable_lock_are_sufficient_bind_recovery_authority() {
    let fixture = DriverFixture::new("driver-atomic-bind-authority");
    let fault_marker = fixture.root.join("fail-flow-revision-publication");
    fs::write(&fault_marker, "fail").unwrap();
    let fault_marker_value = fault_marker.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env_values(
        "run-atomic-bind-authority",
        &[
            (
                "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS",
                &fault_marker_value,
            ),
            (
                "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
                "flow_revision_available",
            ),
        ],
    );
    let package = reviewed_lock_package();
    let request = json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-atomic-bind-authority",
        "flow_lock": package,
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    });

    let failed = fixture.request(request.clone());
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "persistence_failed");
    driver.crash();

    let recovery = load_driver_recovery_state(
        &fixture.run_root("run-atomic-bind-authority"),
        "run-atomic-bind-authority",
    )
    .unwrap();
    assert!(recovery.is_some(), "committed runtime binding was lost");

    fs::remove_file(&fault_marker).unwrap();
    let mut restarted = fixture.spawn("run-atomic-bind-authority");
    let rebound = fixture.request(request);
    assert_eq!(rebound["ok"], true, "{rebound}");
    assert_eq!(rebound["flow_lock_id"], package["lock_id"]);
    assert_eq!(rebound["content_hash"], package["content_hash"]);
    restarted.shutdown();
}

#[test]
fn driver_bind_failure_after_tmux_allocation_exposes_cleanup_pane_identities() {
    let fixture = DriverFixture::new("driver-bind-allocation-cleanup");
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let fault_marker = fixture.root.join("fail-tmux-pane-event");
    fs::write(&fault_marker, "fail").unwrap();
    let fault_marker_value = fault_marker.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env_values(
        "run-bind-cleanup",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            (
                "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS",
                &fault_marker_value,
            ),
            (
                "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
                "tmux_pane_allocated",
            ),
        ],
    );

    let bind_request = json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-bind-cleanup",
        "flow_lock": routed_locked_flow(),
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        },
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    });
    let response = fixture.request(bind_request.clone());

    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(response["error"]["code"], "persistence_failed");
    assert_eq!(
        response["error"]["tmux_cleanup"]["panes"][0],
        json!({
            "activation_id": "root",
            "allocation_generation": 0,
            "pane_id": "%8",
            "session_id": "host-a",
            "window_id": "%7",
            "window_name": "flow-a"
        })
    );

    fs::remove_file(&fault_marker).unwrap();
    let retried = fixture.request(bind_request);
    assert_eq!(retried["ok"], true, "{retried}");
    assert_eq!(retried["run_status"], "paused", "{retried}");
    let resumed = fixture.request(json!({
        "id": "resume-after-allocation-receipt-failure",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-bind-cleanup"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(resumed["tmux_allocations"][0]["activation_id"], "root");
    assert_eq!(resumed["tmux_allocations"][0]["pane_id"], "%8");
    let tmux_log = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert_eq!(tmux_log.matches("new-session ").count(), 1);
    assert!(tmux_log.matches("list-panes -a").count() >= 2);
    let driver_events = fs::read_to_string(fixture.driver_events_path("run-bind-cleanup")).unwrap();
    assert_eq!(
        driver_events
            .matches("\"kind\":\"flow_revision_available\"")
            .count(),
        1
    );
    driver.shutdown();
}

#[test]
fn driver_bind_cleanup_intent_failure_preserves_owned_pane_for_external_cleanup() {
    let fixture = DriverFixture::new("driver-bind-release-outcome");
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let fault_marker = fixture.root.join("fail-release-event");
    let fault_marker_value = fault_marker.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env_values(
        "run-bind-release-outcome",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            (
                "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS",
                &fault_marker_value,
            ),
            (
                "HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT",
                &fault_marker_value,
            ),
        ],
    );

    let response = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-bind-release-outcome",
        "flow_lock": reviewed_lock_package(),
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        },
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));

    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(
        response["error"]["tmux_cleanup"]["panes"][0],
        json!({
            "activation_id": "root",
            "allocation_generation": 0,
            "pane_id": "%8",
            "session_id": "host-a",
            "window_id": "%7",
            "window_name": "flow-a"
        })
    );
    assert!(response["error"].get("tmux_release_outcomes").is_none());
    let tmux_log = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert!(!tmux_log.contains("kill-pane -t host-a:%7.%8"));

    fs::remove_file(fault_marker).unwrap();
    driver.shutdown();
}

#[test]
fn driver_persists_exact_reviewed_flow_lock_package_across_restart() {
    let fixture = DriverFixture::new("driver-exact-flow-lock");
    let mut driver = fixture.spawn("run-exact-lock");
    let package = reviewed_lock_package();
    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-exact-lock",
        "flow_lock": package
    }));

    assert_eq!(bound["ok"], true);
    assert_eq!(bound["flow_lock_id"], package["lock_id"]);
    assert_eq!(bound["content_hash"], package["content_hash"]);
    assert_eq!(bound["flow_lock"]["flow_lock"], package);
    let review_id = bound["flow_lock"]["review_id"].as_str().unwrap();
    assert!(review_id.starts_with("review_"));

    let revision = fixture.single_revision("run-exact-lock");
    assert_eq!(revision["flow_lock"]["lock_id"], package["lock_id"]);
    assert_eq!(
        revision["flow_lock"]["content_hash"],
        package["content_hash"]
    );
    assert_eq!(revision["flow_lock"], package);
    assert_eq!(revision["review_id"], review_id);
    driver.shutdown();

    let mut restarted = fixture.spawn("run-exact-lock");
    let status = fixture.request(json!({
        "id": "status-after-replay",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-exact-lock"
    }));
    assert_eq!(status["ok"], true);
    let revision = &status["context"]["flow_revisions"][0];
    assert_eq!(revision["flow_lock_id"], package["lock_id"]);
    assert_eq!(revision["content_hash"], package["content_hash"]);
    assert_eq!(revision["review"], review_id);
    restarted.shutdown();
}

#[test]
fn driver_tmux_input_ledger_uses_configured_runs_root_not_ambient_environment() {
    let fixture = DriverFixture::new("driver-explicit-ledger-root");
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let ambient_runs_root = fixture.root.join("ambient-runs");
    let ambient_runs_root_value = ambient_runs_root.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env_values(
        "run-explicit-ledger-root",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            ("HUMANIZE_RUNS_DIR", &ambient_runs_root_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "500"),
        ],
    );

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-explicit-ledger-root",
        "flow_lock": reviewed_lock_package(),
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        },
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));

    assert_eq!(bound["ok"], true, "{bound}");
    assert!(
        fixture
            .private_driver_dir("run-explicit-ledger-root")
            .join("machine-inputs.jsonl")
            .exists()
    );
    assert!(
        !fixture
            .run_root("run-explicit-ledger-root")
            .join("machine-inputs.jsonl")
            .exists()
    );
    let ambient_run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(ambient_runs_root))
        .run_root("run-explicit-ledger-root")
        .unwrap();
    assert!(!ambient_run_root.join("machine-inputs.jsonl").exists());
    driver.shutdown();
}

#[test]
fn driver_stop_validation_matches_real_node_contract_requirements() {
    let fixture = DriverFixture::new("driver-real-runtime-stop");
    let mut driver = fixture.spawn("run-runtime-stop");
    assert_eq!(
        fixture.request(json!({
            "id": "bind-run",
            "token": fixture.token,
            "op": "bind_run",
            "run_id": "run-runtime-stop",
            "flow_lock": routed_locked_flow(),
            "review": {
                "review_id": "review-approved",
                "status": "approved"
            }
        }))["ok"],
        true
    );

    let missing = fixture.request(json!({
        "id": "validate-missing",
        "token": fixture.token,
        "op": "validate_stop",
        "run_id": "run-runtime-stop",
        "activation_id": "root"
    }));
    assert_eq!(missing["ok"], false);
    assert_eq!(missing["missing"], json!(["artifact:brief"]));
    assert_eq!(missing["missing_detail"], json!({"artifact_key": "brief"}));

    let delivered = fixture.request(json!({
        "id": "deliver-brief",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-runtime-stop",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": "ready"
    }));
    assert_eq!(delivered["ok"], true);

    let valid = fixture.request(json!({
        "id": "validate-present",
        "token": fixture.token,
        "op": "validate_stop",
        "run_id": "run-runtime-stop",
        "activation_id": "root"
    }));
    assert_eq!(valid["ok"], true);
    driver.shutdown();
}
