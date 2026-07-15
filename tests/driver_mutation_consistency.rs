#[path = "support/driver_flows.rs"]
#[allow(dead_code)]
mod driver_flows;
#[path = "support/driver_tmux.rs"]
mod driver_tmux_support;
#[path = "driver_mutation_consistency/publication.rs"]
mod publication;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::driver::socket_path_for_run_root;
use humanize_plugin::flow::{
    self, ContractArtifact, ContractCompletion, FlowCheckMode, FlowContract, FlowDraft, FlowNode,
    FlowPolicies, FlowPredicate, FlowResource, FlowRoute, NodeAction, NodeDriver, ResourceKind,
};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use driver_flows::{approved_review_id, reviewed_lock_package};
use driver_tmux_support::{ControlledTmuxFixture, capture_identity_is_alive};

#[test]
fn runtime_append_failure_does_not_publish_driver_or_tmux_effects() {
    let fixture = DriverFixture::new("driver-runtime-fault-before-effects");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-runtime-fault-before-effects")
        .unwrap();
    let append_fault = fixture.root.join("fail-runtime-before-effects");
    let append_fault_value = append_fault.to_string_lossy().to_string();
    let fake_tmux = fixture.fake_tmux(true);
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-runtime-fault-before-effects",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            ("HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_AT", "1"),
            (
                "HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_IF_EXISTS",
                &append_fault_value,
            ),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "500"),
        ],
    );
    fixture.bind(
        &mut driver,
        "run-runtime-fault-before-effects",
        manual_flow(NodeDriver::Agent),
        Some(fixture.tmux_request()),
    );
    fs::write(&append_fault, "fail").unwrap();

    let failed = fixture.request(json!({
        "id": "activate-before-runtime-commit",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-runtime-fault-before-effects",
        "node_id": "manual"
    }));

    assert_eq!(failed["ok"], false, "{failed}");
    let calls = fixture.tmux_log_text();
    assert!(!calls.contains("split-window"), "{calls}");
    assert!(!calls.contains("humanize-test-agent"), "{calls}");
    let driver_events = fs::read_to_string(
        fixture
            .private_driver_dir("run-runtime-fault-before-effects")
            .join("driver-events.jsonl"),
    )
    .unwrap();
    assert!(!driver_events.contains("\"activation_id\":\"manual\""));
    driver.shutdown();
}

#[test]
fn activate_pane_failure_keeps_live_and_replay_consistent_then_resume_retries() {
    let fixture = DriverFixture::new("driver-activate-effect-failure");
    let mut driver = fixture.spawn_with_tmux("run-activate-effect-failure");
    fixture.bind(
        &mut driver,
        "run-activate-effect-failure",
        manual_flow(NodeDriver::Human),
        Some(fixture.tmux_request()),
    );
    fs::write(fixture.tmux_failure_marker(), "fail").unwrap();

    let _ = fixture.request(json!({
        "id": "activate-manual",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-activate-effect-failure",
        "node_id": "manual"
    }));

    fixture.assert_replay_and_resume(driver, "run-activate-effect-failure", "manual");
}

#[test]
fn delivery_pane_failure_keeps_live_and_replay_consistent_then_resume_retries() {
    let fixture = DriverFixture::new("driver-delivery-effect-failure");
    let mut driver = fixture.spawn_with_tmux("run-delivery-effect-failure");
    fixture.bind(
        &mut driver,
        "run-delivery-effect-failure",
        routed_flow(),
        Some(fixture.tmux_request()),
    );
    fs::write(fixture.tmux_failure_marker(), "fail").unwrap();

    let _ = fixture.request(json!({
        "id": "deliver-brief",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-delivery-effect-failure",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": "ready"
    }));

    fixture.assert_replay_and_resume(driver, "run-delivery-effect-failure", "follow");
}

#[test]
fn whitespace_only_delivery_evidence_is_rejected_without_runtime_mutation() {
    let fixture = DriverFixture::new("driver-whitespace-delivery-evidence");
    let mut driver = fixture.spawn_with_env("run-whitespace-delivery-evidence", &[]);
    fixture.bind(
        &mut driver,
        "run-whitespace-delivery-evidence",
        manual_flow(NodeDriver::Human),
        None,
    );
    let before = fixture.status("run-whitespace-delivery-evidence");

    let rejected = fixture.request(json!({
        "id": "reject-whitespace-delivery-evidence",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-whitespace-delivery-evidence",
        "delivery_resolution": {
            "started_event_sequence": 1,
            "outcome": "submitted",
            "evidence": " \t\n "
        }
    }));

    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "malformed_request");
    assert_eq!(fixture.status("run-whitespace-delivery-evidence"), before);
    driver.shutdown();
}

#[test]
fn fanout_pane_failure_keeps_live_and_replay_consistent_then_resume_retries() {
    let fixture = DriverFixture::new("driver-fanout-effect-failure");
    let mut driver = fixture.spawn_with_tmux("run-fanout-effect-failure");
    fixture.bind(
        &mut driver,
        "run-fanout-effect-failure",
        fanout_flow(),
        Some(fixture.tmux_request()),
    );
    let delivered = fixture.request(json!({
        "id": "deliver-items",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-fanout-effect-failure",
        "activation_id": "root",
        "artifact_id": "items",
        "payload": "first\nsecond"
    }));
    assert_eq!(delivered["ok"], true, "{delivered}");
    fs::write(fixture.tmux_failure_marker(), "fail").unwrap();

    let _ = fixture.request(json!({
        "id": "fanout-items",
        "token": fixture.token,
        "op": "fanout",
        "run_id": "run-fanout-effect-failure",
        "node_id": "shard",
        "artifact_id": "items"
    }));

    fixture.assert_replay_and_resume(driver, "run-fanout-effect-failure", "shard:items/0");
}

#[test]
fn revision_pane_failure_keeps_live_and_replay_consistent_then_resume_retries() {
    let fixture = DriverFixture::new("driver-revision-effect-failure");
    let mut driver = fixture.spawn_with_tmux("run-revision-effect-failure");
    fixture.bind(
        &mut driver,
        "run-revision-effect-failure",
        dormant_routed_flow(),
        Some(fixture.tmux_request()),
    );
    let delivered = fixture.request(json!({
        "id": "deliver-brief-before-revision",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-revision-effect-failure",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": "ready"
    }));
    assert_eq!(delivered["ok"], true, "{delivered}");
    fs::write(fixture.tmux_failure_marker(), "fail").unwrap();

    let _ = fixture.request(json!({
        "id": "apply-active-route",
        "token": fixture.token,
        "op": "apply_flow_revision",
        "run_id": "run-revision-effect-failure",
        "flow_lock": routed_flow(),
        "apply_mode": "future_activations"
    }));

    fixture.assert_replay_and_resume(driver, "run-revision-effect-failure", "follow");
}

#[test]
fn resume_pane_failure_keeps_live_and_replay_consistent_then_resume_retries() {
    let fixture = DriverFixture::new("driver-resume-effect-failure");
    let mut driver = fixture.spawn_with_tmux("run-resume-effect-failure");
    fixture.bind(
        &mut driver,
        "run-resume-effect-failure",
        routed_flow(),
        Some(fixture.tmux_request()),
    );
    let paused = fixture.request(json!({
        "id": "pause-run",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-resume-effect-failure"
    }));
    assert_eq!(paused["ok"], true, "{paused}");
    let delivered = fixture.request(json!({
        "id": "deliver-while-paused",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-resume-effect-failure",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": "ready"
    }));
    assert_eq!(delivered["ok"], true, "{delivered}");
    fs::write(fixture.tmux_failure_marker(), "fail").unwrap();

    let _ = fixture.request(json!({
        "id": "resume-with-pane-failure",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-resume-effect-failure"
    }));

    fixture.assert_replay_and_resume(driver, "run-resume-effect-failure", "follow");
}

#[test]
fn post_launch_persistence_failure_replays_and_resume_does_not_relaunch() {
    let fixture = DriverFixture::new("driver-post-launch-persistence");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-post-launch-persistence")
        .unwrap();
    let driver_event_fault = fixture.root.join("fail-agent-launched-event");
    let driver_event_fault_value = driver_event_fault.to_string_lossy().to_string();
    let fake_tmux = fixture.fake_tmux(true);
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-post-launch-persistence",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            (
                "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS",
                &driver_event_fault_value,
            ),
            (
                "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
                "agent_launch_submitted",
            ),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "500"),
        ],
    );
    fixture.bind(
        &mut driver,
        "run-post-launch-persistence",
        manual_flow(NodeDriver::Agent),
        Some(fixture.tmux_request()),
    );
    fs::write(&driver_event_fault, "fail").unwrap();

    let failed = fixture.request(json!({
        "id": "activate-agent",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-post-launch-persistence",
        "node_id": "manual"
    }));
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(
        fixture.agent_launch_count(),
        1,
        "response={failed}; tmux_log={}",
        fixture.tmux_log_text()
    );
    let live = fixture.status("run-post-launch-persistence");
    assert!(live["context"]["activations"]["manual"].is_object());
    driver.crash();

    fs::remove_file(&driver_event_fault).unwrap();
    let mut restarted = fixture.spawn_with_env(
        "run-post-launch-persistence",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            (
                "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS",
                &driver_event_fault_value,
            ),
            (
                "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
                "agent_launch_submitted",
            ),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "500"),
        ],
    );
    let replayed = fixture.status("run-post-launch-persistence");
    assert_eq!(replayed["event_cursor"], live["event_cursor"]);
    assert_eq!(replayed["run_status"], "paused");
    assert_runtime_context_except_control(&replayed, &live);
    let started_event_sequence = replayed["context"]["ambiguous_deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|delivery| delivery["role"] == "agent_launch")
        .unwrap()["started_event_sequence"]
        .as_u64()
        .unwrap();

    let retried = fixture.request(json!({
        "id": "resume-reconciliation",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-post-launch-persistence",
        "delivery_resolution": {
            "started_event_sequence": started_event_sequence,
            "outcome": "submitted",
            "evidence": "receiver session was observed running after Enter"
        }
    }));
    assert_eq!(retried["ok"], true, "{retried}");
    assert_eq!(fixture.agent_launch_count(), 1);
    assert!(fixture.tmux_log_text().contains("Inspect the manual node."));
    restarted.shutdown();
}

#[test]
fn process_death_after_enter_exposes_ambiguous_delivery_without_resend() {
    let fixture = DriverFixture::new("driver-enter-death-ambiguity");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-enter-death-ambiguity")
        .unwrap();
    let killing_tmux = fixture.fake_tmux_kills_driver_after_agent_enter();
    let killing_tmux_value = killing_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-enter-death-ambiguity",
        &[
            ("HUMANIZE_TMUX_BIN", &killing_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
        ],
    );
    fixture.bind(
        &mut driver,
        "run-enter-death-ambiguity",
        manual_flow(NodeDriver::Agent),
        Some(fixture.tmux_request()),
    );

    let response = fixture.request_until_disconnect(json!({
        "id": "activate-agent-before-death",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-enter-death-ambiguity",
        "node_id": "manual"
    }));
    assert!(response.is_empty(), "{response}");
    driver.wait_for_exit(Duration::from_secs(2));
    assert_eq!(fixture.agent_launch_count(), 1);

    let mut restarted = fixture.spawn_with_env(
        "run-enter-death-ambiguity",
        &[
            ("HUMANIZE_TMUX_BIN", &killing_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
        ],
    );
    let ambiguous = fixture.status("run-enter-death-ambiguity");
    assert_eq!(
        ambiguous["context"]["ambiguous_deliveries"][0]["activation_id"],
        "manual"
    );
    assert_eq!(
        ambiguous["context"]["ambiguous_deliveries"][0]["role"],
        "agent_launch"
    );
    assert_eq!(
        ambiguous["context"]["ambiguous_deliveries"][0]["reason"],
        "submission_receipt_incomplete"
    );
    let first_started_event_sequence =
        ambiguous["context"]["ambiguous_deliveries"][0]["started_event_sequence"]
            .as_u64()
            .unwrap();

    let retry_response = fixture.request_until_disconnect(json!({
        "id": "resolve-first-and-retry-delivery",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-enter-death-ambiguity",
        "delivery_resolution": {
            "started_event_sequence": first_started_event_sequence,
            "outcome": "not_submitted",
            "evidence": "receiver session did not start"
        }
    }));
    assert!(retry_response.is_empty(), "{retry_response}");
    restarted.wait_for_exit(Duration::from_secs(2));
    assert_eq!(fixture.agent_launch_count(), 2);

    let stable_tmux = fixture.fake_tmux(false);
    let stable_tmux_value = stable_tmux.to_string_lossy().to_string();
    let mut replayed = fixture.spawn_with_env(
        "run-enter-death-ambiguity",
        &[
            ("HUMANIZE_TMUX_BIN", &stable_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
        ],
    );
    let resumed = fixture.request(json!({
        "id": "resume-new-ambiguous-delivery",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-enter-death-ambiguity"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(fixture.agent_launch_count(), 2, "{resumed}");
    assert_eq!(
        resumed["actuation"]["warnings"][0]["status"],
        "ambiguous_delivery"
    );
    let before_stale = fixture.status("run-enter-death-ambiguity");
    let second_started_event_sequence =
        before_stale["context"]["ambiguous_deliveries"][0]["started_event_sequence"]
            .as_u64()
            .unwrap();
    assert!(second_started_event_sequence > first_started_event_sequence);

    let stale = fixture.request(json!({
        "id": "replay-stale-delivery-resolution",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-enter-death-ambiguity",
        "delivery_resolution": {
            "started_event_sequence": first_started_event_sequence,
            "outcome": "not_submitted",
            "evidence": "stale replay for the first attempt"
        }
    }));
    assert_eq!(stale["ok"], false, "{stale}");
    assert_eq!(stale["error"]["code"], "delivery_barrier_conflict");
    assert_eq!(fixture.status("run-enter-death-ambiguity"), before_stale);
    assert_eq!(fixture.agent_launch_count(), 2);

    let resolved = fixture.request(json!({
        "id": "resolve-current-and-retry-delivery",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-enter-death-ambiguity",
        "delivery_resolution": {
            "started_event_sequence": second_started_event_sequence,
            "outcome": "not_submitted",
            "evidence": "receiver session did not start on the second attempt"
        }
    }));
    assert_eq!(resolved["ok"], true, "{resolved}");
    assert_eq!(fixture.agent_launch_count(), 3);
    let after_resolution = fixture.status("run-enter-death-ambiguity");
    assert_eq!(
        after_resolution["context"]["ambiguous_deliveries"],
        json!([])
    );
    replayed.shutdown();
}

#[test]
fn bind_crash_before_and_after_pane_ownership_replays_without_duplicate_input() {
    for (stage, suffix) in [("before_pane", "before"), ("after_pane", "after")] {
        let fixture = DriverFixture::new(&format!("driver-bind-crash-{suffix}"));
        let run_id = format!("run-bind-crash-{suffix}");
        let crashing_tmux = fixture.fake_tmux_for_bind_crash(stage);
        let crashing_tmux_value = crashing_tmux.to_string_lossy().to_string();
        let mut driver = fixture.spawn_with_env(
            &run_id,
            &[
                ("HUMANIZE_TMUX_BIN", &crashing_tmux_value),
                ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "500"),
            ],
        );
        let request = fixture.initial_agent_bind_request(&run_id);

        let response = fixture.request_until_disconnect(request.clone());
        assert!(response.is_empty(), "stage={stage}; response={response}");
        driver.wait_for_exit(Duration::from_secs(2));

        let stable_tmux = fixture.fake_tmux(true);
        let stable_tmux_value = stable_tmux.to_string_lossy().to_string();
        let mut restarted = fixture.spawn_with_env(
            &run_id,
            &[
                ("HUMANIZE_TMUX_BIN", &stable_tmux_value),
                ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "500"),
            ],
        );
        let recovered = fixture.request(request);
        assert_eq!(recovered["ok"], true, "stage={stage}; {recovered}");
        assert_eq!(recovered["run_status"], "paused");
        let resumed = fixture.request(json!({
            "id": "resume-after-bind-crash",
            "token": fixture.token,
            "op": "resume",
            "run_id": run_id
        }));
        assert_eq!(resumed["ok"], true, "stage={stage}; {resumed}");
        assert_eq!(
            fixture.agent_launch_count(),
            1,
            "stage={stage}; tmux_log={}",
            fixture.tmux_log_text()
        );
        assert_eq!(
            fixture.tmux_log_text().matches("Create the brief.").count(),
            1,
            "stage={stage}; tmux_log={}",
            fixture.tmux_log_text()
        );
        restarted.shutdown();
    }
}

#[test]
fn bind_death_after_agent_enter_exposes_ambiguity_without_resend() {
    let fixture = DriverFixture::new("driver-bind-agent-enter-death");
    let run_id = "run-bind-agent-enter-death";
    let crashing_tmux = fixture.fake_tmux_for_bind_crash("agent_enter");
    let crashing_tmux_value = crashing_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(run_id, &[("HUMANIZE_TMUX_BIN", &crashing_tmux_value)]);
    let request = fixture.initial_agent_bind_request(run_id);

    let response = fixture.request_until_disconnect(request.clone());
    assert!(response.is_empty(), "{response}");
    driver.wait_for_exit(Duration::from_secs(2));
    assert_eq!(fixture.agent_launch_count(), 1);

    let stable_tmux = fixture.fake_tmux(false);
    let stable_tmux_value = stable_tmux.to_string_lossy().to_string();
    let mut restarted =
        fixture.spawn_with_env(run_id, &[("HUMANIZE_TMUX_BIN", &stable_tmux_value)]);
    let recovered = fixture.request(request);
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(
        recovered["tmux"]["actuation"]["warnings"][0]["role"],
        "agent_launch"
    );
    assert_eq!(fixture.agent_launch_count(), 1);
    restarted.shutdown();
}

#[test]
fn bind_death_after_prompt_enter_exposes_ambiguity_without_resend() {
    let fixture = DriverFixture::new("driver-bind-prompt-enter-death");
    let run_id = "run-bind-prompt-enter-death";
    let crashing_tmux = fixture.fake_tmux_for_bind_crash("prompt_enter");
    let crashing_tmux_value = crashing_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        run_id,
        &[
            ("HUMANIZE_TMUX_BIN", &crashing_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "500"),
        ],
    );
    let request = fixture.initial_agent_bind_request(run_id);

    let response: Value =
        serde_json::from_str(&fixture.request_until_disconnect(request.clone())).unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["tmux"]["actuation"]["warnings"][0]["status"],
        "readiness_pending"
    );
    driver.wait_for_exit(Duration::from_secs(4));
    assert_eq!(fixture.agent_launch_count(), 1);
    assert_eq!(
        fixture.tmux_log_text().matches("Create the brief.").count(),
        1
    );

    let stable_tmux = fixture.fake_tmux(false);
    let stable_tmux_value = stable_tmux.to_string_lossy().to_string();
    let mut restarted =
        fixture.spawn_with_env(run_id, &[("HUMANIZE_TMUX_BIN", &stable_tmux_value)]);
    let recovered = fixture.request(request);
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(
        recovered["tmux"]["actuation"]["warnings"][0]["role"],
        "node_prompt"
    );
    assert_eq!(fixture.agent_launch_count(), 1);
    assert_eq!(
        fixture.tmux_log_text().matches("Create the brief.").count(),
        1
    );
    restarted.shutdown();
}

#[test]
fn prompt_death_exposes_barrier_and_submitted_resolution_survives_restart() {
    let fixture = DriverFixture::new("driver-prompt-death-ambiguity");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-prompt-death-ambiguity")
        .unwrap();
    let killing_tmux = fixture.fake_tmux_kills_driver_after_prompt_enter();
    let killing_tmux_value = killing_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-prompt-death-ambiguity",
        &[
            ("HUMANIZE_TMUX_BIN", &killing_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "500"),
        ],
    );
    fixture.bind(
        &mut driver,
        "run-prompt-death-ambiguity",
        manual_flow(NodeDriver::Agent),
        Some(fixture.tmux_request()),
    );

    let response: Value = serde_json::from_str(&fixture.request_until_disconnect(json!({
        "id": "activate-agent-before-prompt-death",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-prompt-death-ambiguity",
        "node_id": "manual"
    })))
    .unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["actuation"]["warnings"][0]["status"],
        "readiness_pending"
    );
    driver.wait_for_exit(Duration::from_secs(4));
    assert_eq!(fixture.agent_launch_count(), 1);
    assert_eq!(
        fixture
            .tmux_log_text()
            .matches("Inspect the manual node.")
            .count(),
        1
    );

    let stable_tmux = fixture.fake_tmux(false);
    let stable_tmux_value = stable_tmux.to_string_lossy().to_string();
    let mut restarted = fixture.spawn_with_env(
        "run-prompt-death-ambiguity",
        &[
            ("HUMANIZE_TMUX_BIN", &stable_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
        ],
    );
    let resumed = fixture.request(json!({
        "id": "resume-prompt-ambiguity",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-prompt-death-ambiguity"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(resumed["actuation"]["warnings"][0]["role"], "node_prompt");
    assert_eq!(fixture.agent_launch_count(), 1);
    assert_eq!(
        fixture
            .tmux_log_text()
            .matches("Inspect the manual node.")
            .count(),
        1
    );
    let ambiguous = fixture.status("run-prompt-death-ambiguity");
    let started_event_sequence =
        ambiguous["context"]["ambiguous_deliveries"][0]["started_event_sequence"]
            .as_u64()
            .unwrap();

    let resolved = fixture.request(json!({
        "id": "resolve-prompt-as-submitted",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-prompt-death-ambiguity",
        "delivery_resolution": {
            "started_event_sequence": started_event_sequence,
            "outcome": "submitted",
            "evidence": "receiver acknowledged the prompt transaction"
        }
    }));
    assert_eq!(resolved["ok"], true, "{resolved}");
    assert_eq!(
        resolved["delivery_resolution"]["started_event_sequence"],
        started_event_sequence
    );
    assert_eq!(resolved["delivery_resolution"]["role"], "node_prompt");
    assert_eq!(resolved["delivery_resolution"]["outcome"], "submitted");
    assert_eq!(
        fixture.status("run-prompt-death-ambiguity")["context"]["ambiguous_deliveries"],
        json!([])
    );
    restarted.crash();

    let calls_before_second_restart = fixture.tmux_log_text();
    let mut replayed = fixture.spawn_with_env(
        "run-prompt-death-ambiguity",
        &[("HUMANIZE_TMUX_BIN", &stable_tmux_value)],
    );
    assert_eq!(
        fixture.status("run-prompt-death-ambiguity")["context"]["ambiguous_deliveries"],
        json!([])
    );
    let resumed_after_restart = fixture.request(json!({
        "id": "resume-after-submitted-resolution-replay",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-prompt-death-ambiguity"
    }));
    assert_eq!(resumed_after_restart["ok"], true, "{resumed_after_restart}");
    let input_before = calls_before_second_restart
        .lines()
        .filter(|line| line.starts_with("send-keys "))
        .collect::<Vec<_>>();
    let calls_after_second_restart = fixture.tmux_log_text();
    let input_after = calls_after_second_restart
        .lines()
        .filter(|line| line.starts_with("send-keys "))
        .collect::<Vec<_>>();
    assert_eq!(input_after, input_before);
    replayed.shutdown();
}

#[test]
fn fixture_drop_stops_and_waits_capture_helpers_after_driver_crash() {
    let (root, identity) = {
        let fixture = DriverFixture::new("driver-fixture-capture-cleanup");
        let fake_tmux = fixture.fake_tmux(false);
        let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
        let mut driver = fixture.spawn_with_env(
            "run-fixture-capture-cleanup",
            &[("HUMANIZE_TMUX_BIN", &fake_tmux_value)],
        );
        fixture.bind(
            &mut driver,
            "run-fixture-capture-cleanup",
            reviewed_lock_package(),
            Some(fixture.tmux_request()),
        );
        let root = fixture.root.clone();
        let identity = fixture.tmux_control.capture_identity("8").unwrap();
        assert!(capture_identity_is_alive(&identity));
        driver.crash();
        (root, identity)
    };

    assert!(!capture_identity_is_alive(&identity));
    assert!(
        !root.exists(),
        "fixture root remained at {}",
        root.display()
    );
}

struct DriverFixture {
    root: PathBuf,
    token: &'static str,
    tmux_control: ControlledTmuxFixture,
}

impl DriverFixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .join("hpd-mut")
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

    fn spawn_with_tmux(&self, run_id: &str) -> DriverProcess {
        let fake_tmux = self.fake_tmux(false);
        let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
        self.spawn_with_env(run_id, &[("HUMANIZE_TMUX_BIN", &fake_tmux_value)])
    }

    fn spawn_with_env(&self, run_id: &str, envs: &[(&str, &str)]) -> DriverProcess {
        let mut command = self.driver_command(run_id, envs);
        let mut child = command.spawn().unwrap();
        wait_for_socket(&mut child, &self.socket_path(run_id));
        DriverProcess { child }
    }

    fn rejected_restart(&self, run_id: &str) -> String {
        let mut command = self.driver_command(run_id, &[]);
        let mut child = command.spawn().unwrap();
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() >= Duration::from_secs(2) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("driver accepted an unsafe publication directory");
            }
            thread::sleep(Duration::from_millis(20));
        };
        let mut stderr = String::new();
        child
            .stderr
            .as_mut()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(!status.success(), "unsafe restart unexpectedly succeeded");
        stderr
    }

    fn driver_command(&self, run_id: &str, envs: &[(&str, &str)]) -> Command {
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
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("HUMANIZE_STATE_ROOT", &self.root);
        for (key, value) in envs {
            command.env(key, value);
        }
        command
    }

    fn bind(
        &self,
        _driver: &mut DriverProcess,
        run_id: &str,
        flow_lock: Value,
        tmux: Option<Value>,
    ) -> Value {
        let mut request = json!({
            "id": "bind-run",
            "token": self.token,
            "op": "bind_run",
            "run_id": run_id,
            "flow_lock": flow_lock
        });
        if let Some(tmux) = tmux {
            request["tmux"] = tmux;
        }
        let response = self.request(request);
        assert_eq!(response["ok"], true, "{response}");
        response
    }

    fn assert_replay_and_resume(
        &self,
        mut driver: DriverProcess,
        run_id: &str,
        activation_id: &str,
    ) {
        let live = self.status(run_id);
        assert!(live["context"]["activations"][activation_id].is_object());
        driver.crash();

        let mut restarted = self.spawn_with_tmux(run_id);
        let replayed = self.status(run_id);
        assert_eq!(replayed["event_cursor"], live["event_cursor"]);
        assert_eq!(replayed["context_generation"], live["context_generation"]);
        assert_eq!(replayed["run_status"], "paused");
        assert_runtime_context_except_control(&replayed, &live);

        fs::remove_file(self.tmux_failure_marker()).unwrap();
        let retried = self.request(json!({
            "id": "resume-reconciliation",
            "token": self.token,
            "op": "resume",
            "run_id": run_id
        }));
        assert_eq!(retried["ok"], true, "{retried}");
        assert!(
            retried["tmux_allocations"]
                .as_array()
                .is_some_and(|allocations| allocations.iter().any(|allocation| {
                    allocation["activation_id"].as_str() == Some(activation_id)
                })),
            "{retried}"
        );
        restarted.shutdown();
    }

    fn request(&self, request: Value) -> Value {
        let request = self.with_review_id(request);
        let run_id = request["run_id"].as_str().unwrap();
        let mut stream = UnixStream::connect(self.socket_path(run_id)).unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        self.wait_for_hook_helpers();
        serde_json::from_str(&response).unwrap()
    }

    fn request_until_disconnect(&self, request: Value) -> String {
        let request = self.with_review_id(request);
        let run_id = request["run_id"].as_str().unwrap();
        let mut stream = UnixStream::connect(self.socket_path(run_id)).unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        self.wait_for_hook_helpers();
        response
    }

    fn wait_for_hook_helpers(&self) {
        assert!(self.tmux_control.wait_for_hooks());
    }

    fn status(&self, run_id: &str) -> Value {
        self.request(json!({
            "id": "status",
            "token": self.token,
            "op": "status",
            "run_id": run_id
        }))
    }

    fn tmux_request(&self) -> Value {
        json!({
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        })
    }

    fn initial_agent_bind_request(&self, run_id: &str) -> Value {
        json!({
            "id": "bind-initial-agent",
            "token": self.token,
            "op": "bind_run",
            "run_id": run_id,
            "flow_lock": reviewed_lock_package(),
            "tmux": self.tmux_request()
        })
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

    fn snapshot_path(&self, run_id: &str) -> PathBuf {
        self.private_driver_dir(run_id).join("snapshot.json")
    }

    fn public_journal(&self, run_id: &str) -> Vec<u8> {
        fs::read(self.run_root(run_id).join("records/events.jsonl")).unwrap_or_default()
    }

    fn public_journal_events(&self, run_id: &str) -> Vec<Value> {
        String::from_utf8(self.public_journal(run_id))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn public_manifest(&self, run_id: &str) -> Value {
        serde_json::from_slice(&fs::read(self.run_root(run_id).join("manifest.json")).unwrap())
            .unwrap()
    }

    fn pending_publication_count(&self, run_id: &str) -> usize {
        directory_entry_count(&self.private_driver_dir(run_id).join("publication-outbox"))
    }

    fn published_publication_count(&self, run_id: &str) -> usize {
        directory_entry_count(&self.private_driver_dir(run_id).join("publication-ledger"))
    }

    fn tmux_failure_marker(&self) -> PathBuf {
        self.root.join("fail-split-window")
    }

    fn tmux_log_text(&self) -> String {
        fs::read_to_string(self.root.join("tmux.log")).unwrap_or_default()
    }

    fn agent_launch_count(&self) -> usize {
        self.tmux_log_text()
            .lines()
            .filter(|line| line.contains("humanize-test-agent"))
            .count()
    }

    fn fake_tmux(&self, agent_ready: bool) -> PathBuf {
        let path = self.root.join(if agent_ready {
            "fake-tmux-agent-ready"
        } else {
            "fake-tmux-pane-failure"
        });
        let script = format!(
            r#"#!/bin/sh
root='{}'
agent_ready='{}'
printf '%s\n' "$*" >> "$root/tmux.log"
target=''
previous=''
last=''
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
      HUMANIZE_READY_RUN_ID=*|HUMANIZE_READY_ACTIVATION_ID=*|HUMANIZE_READY_ALLOCATION_GENERATION=*|HUMANIZE_READY_NONCE=*|HUMANIZE_PARTICIPANT_RUN_ID=*|HUMANIZE_PARTICIPANT_ACTIVATION_ID=*|HUMANIZE_PARTICIPANT_HANDLE=*|HUMANIZE_PARTICIPANT_CREDENTIAL=*|HUMANIZE_PARTICIPANT_BINDING_FILE=*) export "$1"; shift ;;
      *) break ;;
    esac
  done
}}
case "$1" in
  has-session)
    exit 1
    ;;
  new-session)
    printf '%s\t%s\n' '%7' '%8'
    ;;
  split-window)
    if test -f "$root/fail-split-window"; then
      exit 41
    fi
    printf '%s\n' '%9'
    ;;
  display-message)
    pane="${{target##*.}}"
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
        if test "$agent_ready" = 'yes'; then
          load_ready_environment
          pending="$root/hook-helper-${{target##*.}}-$$.pending"
          done="${{pending%.pending}}.done"
          : > "$pending"
          (
            printf '%s\n' '{{"hook_event_name":"SessionStart","session_id":"fake-native-session"}}' |
              HUMANIZE_RUNS_DIR="$root/runs" TMUX_PANE="${{target##*.}}" '{}' --agent-ready-hook --source codex_session_start
            mv "$pending" "$done"
          ) </dev/null >> "$root/hook.out" 2>> "$root/hook.err" &
        fi
        ;;
    esac
    ;;
  kill-pane)
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" stop "$root" "${{pane#%}}"
    ;;
esac
exit 0
"#,
            self.root.display(),
            if agent_ready { "yes" } else { "no" },
            env!("CARGO_BIN_EXE_humanize-plugin-mcp")
        );
        fs::write(&path, script).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn fake_tmux_for_bind_crash(&self, stage: &str) -> PathBuf {
        let path = self.root.join(format!("fake-tmux-bind-crash-{stage}"));
        let script = format!(
            r#"#!/bin/sh
root='{}'
stage='{}'
printf '%s\n' "$*" >> "$root/tmux.log"
target=''
previous=''
last=''
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
      HUMANIZE_READY_RUN_ID=*|HUMANIZE_READY_ACTIVATION_ID=*|HUMANIZE_READY_ALLOCATION_GENERATION=*|HUMANIZE_READY_NONCE=*|HUMANIZE_PARTICIPANT_RUN_ID=*|HUMANIZE_PARTICIPANT_ACTIVATION_ID=*|HUMANIZE_PARTICIPANT_HANDLE=*|HUMANIZE_PARTICIPANT_CREDENTIAL=*|HUMANIZE_PARTICIPANT_BINDING_FILE=*) export "$1"; shift ;;
      *) break ;;
    esac
  done
}}
case "$1" in
  has-session)
    if test "$stage" = 'before_pane'; then kill -KILL "$PPID"; fi
    exit 1
    ;;
  new-session)
    printf '%s\t%s\n' '%7' '%8'
    ;;
  split-window)
    printf '%s\n' '%9'
    ;;
  display-message)
    pane="${{target##*.}}"
    printf '%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' "$pane"
    ;;
  pipe-pane)
    if test "$stage" = 'after_pane'; then kill -KILL "$PPID"; fi
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" start "$root" "${{pane#%}}" "$last"
    ;;
  capture-pane)
    printf 'final capture for %s\n' "$target"
    ;;
  set-buffer)
    case "$last" in
      *'Create the brief.'*)
        : > "$root/prompt-input-started"
        ;;
    esac
    ;;
  send-keys)
    case "$*" in
      *humanize-test-agent*)
        : > "$root/agent-input-started"
        load_ready_environment
        pending="$root/hook-helper-${{target##*.}}-$$.pending"
        done="${{pending%.pending}}.done"
        : > "$pending"
        (
          printf '%s\n' '{{"hook_event_name":"SessionStart","session_id":"fake-native-session"}}' |
            HUMANIZE_RUNS_DIR="$root/runs" TMUX_PANE="${{target##*.}}" '{}' --agent-ready-hook --source codex_session_start
          mv "$pending" "$done"
        ) </dev/null >> "$root/hook.out" 2>> "$root/hook.err" &
        ;;
      *'Create the brief.'*)
        : > "$root/prompt-input-started"
        ;;
    esac
    if test "$last" = 'Enter' && test -f "$root/agent-input-started"; then
      rm -f "$root/agent-input-started"
      if test "$stage" = 'agent_enter'; then kill -KILL "$PPID"; fi
    fi
    if test "$last" = 'Enter' && test -f "$root/prompt-input-started"; then
      rm -f "$root/prompt-input-started"
      if test "$stage" = 'prompt_enter'; then kill -KILL "$PPID"; fi
    fi
    ;;
  kill-pane)
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" stop "$root" "${{pane#%}}"
    ;;
esac
exit 0
"#,
            self.root.display(),
            stage,
            env!("CARGO_BIN_EXE_humanize-plugin-mcp")
        );
        fs::write(&path, script).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn fake_tmux_kills_driver_after_agent_enter(&self) -> PathBuf {
        let path = self.root.join("fake-tmux-kill-after-enter");
        let script = format!(
            r#"#!/bin/sh
root='{}'
printf '%s\n' "$*" >> "$root/tmux.log"
target=''
previous=''
last=''
for arg in "$@"; do
  if test "$previous" = '-t'; then target="$arg"; fi
  previous="$arg"
  last="$arg"
done
case "$1" in
  has-session)
    exit 1
    ;;
  new-session)
    printf '%s\t%s\n' '%7' '%8'
    ;;
  split-window)
    printf '%s\n' '%9'
    ;;
  display-message)
    pane="${{target##*.}}"
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
    case "$target:$*" in
      *.%9:*' Enter')
        kill -KILL "$PPID"
        ;;
    esac
    ;;
  kill-pane)
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" stop "$root" "${{pane#%}}"
    ;;
esac
exit 0
"#,
            self.root.display()
        );
        fs::write(&path, script).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn fake_tmux_kills_driver_after_prompt_enter(&self) -> PathBuf {
        let path = self.root.join("fake-tmux-kill-after-prompt-enter");
        let prompt_marker = self.root.join("prompt-input-started");
        let script = format!(
            r#"#!/bin/sh
root='{}'
printf '%s\n' "$*" >> "$root/tmux.log"
target=''
previous=''
last=''
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
      HUMANIZE_READY_RUN_ID=*|HUMANIZE_READY_ACTIVATION_ID=*|HUMANIZE_READY_ALLOCATION_GENERATION=*|HUMANIZE_READY_NONCE=*|HUMANIZE_PARTICIPANT_RUN_ID=*|HUMANIZE_PARTICIPANT_ACTIVATION_ID=*|HUMANIZE_PARTICIPANT_HANDLE=*|HUMANIZE_PARTICIPANT_CREDENTIAL=*|HUMANIZE_PARTICIPANT_BINDING_FILE=*) export "$1"; shift ;;
      *) break ;;
    esac
  done
}}
case "$1" in
  has-session)
    exit 1
    ;;
  new-session)
    printf '%s\t%s\n' '%7' '%8'
    ;;
  split-window)
    printf '%s\n' '%9'
    ;;
  display-message)
    pane="${{target##*.}}"
    printf '%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' "$pane"
    ;;
  pipe-pane)
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" start "$root" "${{pane#%}}" "$last"
    ;;
  capture-pane)
    printf 'final capture for %s\n' "$target"
    ;;
  set-buffer)
    case "$last" in
      *'Inspect the manual node.'*)
        : > '{}'
        ;;
    esac
    ;;
  send-keys)
    case "$*" in
      *humanize-test-agent*)
        load_ready_environment
        pending="$root/hook-helper-${{target##*.}}-$$.pending"
        done="${{pending%.pending}}.done"
        : > "$pending"
        (
          printf '%s\n' '{{"hook_event_name":"SessionStart","session_id":"fake-native-session"}}' |
            HUMANIZE_RUNS_DIR="$root/runs" TMUX_PANE="${{target##*.}}" '{}' --agent-ready-hook --source codex_session_start
          mv "$pending" "$done"
        ) </dev/null >> "$root/hook.out" 2>> "$root/hook.err" &
        ;;
      *'Inspect the manual node.'*)
        : > '{}'
        ;;
    esac
    if test "$last" = 'Enter' && test -f '{}'; then
      kill -KILL "$PPID"
    fi
    ;;
  kill-pane)
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" stop "$root" "${{pane#%}}"
    ;;
esac
exit 0
        "#,
            self.root.display(),
            prompt_marker.display(),
            env!("CARGO_BIN_EXE_humanize-plugin-mcp"),
            prompt_marker.display(),
            prompt_marker.display()
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

fn directory_entry_count(path: &Path) -> usize {
    fs::read_dir(path)
        .map(|entries| entries.count())
        .unwrap_or_default()
}

impl Drop for DriverFixture {
    fn drop(&mut self) {
        let _ = self.tmux_control.stop_all();
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct DriverProcess {
    child: Child,
}

impl DriverProcess {
    fn shutdown(&mut self) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, "shutdown");
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

    fn wait_for_exit(&mut self, timeout: Duration) {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        panic!("driver did not exit before timeout");
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
            let stderr = child
                .stderr
                .as_mut()
                .map(|stderr| {
                    let mut output = String::new();
                    let _ = std::io::Read::read_to_string(stderr, &mut output);
                    output
                })
                .unwrap_or_default();
            panic!("driver exited before socket was ready: {status}; stderr={stderr}");
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("driver socket was not ready at {}", path.display());
}

fn assert_runtime_context_except_control(actual: &Value, expected: &Value) {
    let mut actual = actual["context"].clone();
    let mut expected = expected["context"].clone();
    for context in [&mut actual, &mut expected] {
        if let Some(object) = context.as_object_mut() {
            object.remove("run_status");
            object.remove("run_status_reason");
            object.remove("event_cursor");
            object.remove("context_generation");
        }
    }
    assert_eq!(actual, expected);
}

fn routed_flow() -> Value {
    flow_package("brief", "brief", "follow", NodeDriver::Human)
}

fn dormant_routed_flow() -> Value {
    flow_package("never", "brief", "follow", NodeDriver::Human)
}

fn manual_flow(driver: NodeDriver) -> Value {
    flow_package("never", "brief", "manual", driver)
}

fn fanout_flow() -> Value {
    flow_package("never", "items", "shard", NodeDriver::Human)
}

fn flow_package(
    predicate_artifact: &str,
    root_artifact: &str,
    target_node: &str,
    target_driver: NodeDriver,
) -> Value {
    let prompt_resource = (target_driver != NodeDriver::Human).then(|| FlowResource {
        id: format!("prompt.{target_node}"),
        kind: ResourceKind::Prompt,
        source: format!("inline:Inspect the {target_node} node."),
    });
    let mut resources = vec![
        FlowResource {
            id: "README.md".into(),
            kind: ResourceKind::Readme,
            source: "inline:Driver mutation consistency fixture.".into(),
        },
        FlowResource {
            id: format!("schema.root.{root_artifact}"),
            kind: ResourceKind::Schema,
            source: "inline:text".into(),
        },
    ];
    resources.extend(prompt_resource);
    let draft = FlowDraft {
        nodes: vec![
            FlowNode {
                id: "root".into(),
                contract_id: Some("contract.root".into()),
                action: Some(NodeAction {
                    driver: NodeDriver::Human,
                    prompt_ref: None,
                    resource_refs: Vec::new(),
                    reads: Vec::new(),
                    writes: vec![format!("artifact.{root_artifact}")],
                    verdict_artifact: None,
                }),
                write_scopes: Vec::new(),
                extensions: Vec::new(),
            },
            FlowNode {
                id: target_node.into(),
                action: Some(NodeAction {
                    driver: target_driver,
                    prompt_ref: (target_driver != NodeDriver::Human)
                        .then(|| format!("prompt.{target_node}")),
                    resource_refs: Vec::new(),
                    reads: vec![format!("artifact.{root_artifact}")],
                    writes: Vec::new(),
                    verdict_artifact: None,
                }),
                ..FlowNode::default()
            },
        ],
        contracts: vec![FlowContract {
            id: "contract.root".into(),
            completion: Some(ContractCompletion::AllArtifacts),
            artifacts: vec![ContractArtifact {
                id: root_artifact.into(),
                schema_resource_id: Some(format!("schema.root.{root_artifact}")),
            }],
        }],
        routes: vec![FlowRoute {
            predicate: FlowPredicate::exists_artifact(predicate_artifact).unwrap(),
            for_each: None,
            activate: target_node.into(),
        }],
        resources,
        imports: Vec::new(),
        policies: FlowPolicies::default(),
        extensions: Vec::new(),
    };
    let lock = flow::flow_lock(&draft, FlowCheckMode::Core).unwrap();
    serde_json::to_value(lock).unwrap()
}
