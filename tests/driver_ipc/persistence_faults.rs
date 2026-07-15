use std::fs;

use serde_json::json;

use super::driver_flows::locked_flow;
use super::support::DriverFixture;

#[test]
fn driver_status_exposes_cursor_generation_and_rejects_stale_mutations() {
    let fixture = DriverFixture::new("driver-cursor-conflict");
    let mut driver = fixture.spawn("run-cursor");

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-cursor",
        "flow_lock": locked_flow(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    assert!(bound["event_cursor"].as_u64().unwrap() > 0);
    assert!(bound["context_generation"].as_u64().unwrap() > 0);

    let status = fixture.request(json!({
        "id": "status",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-cursor"
    }));
    assert_eq!(status["ok"], true);
    let cursor = status["event_cursor"].as_u64().unwrap();
    let generation = status["context_generation"].as_u64().unwrap();
    assert_eq!(status["context"]["event_cursor"], json!(cursor));
    assert_eq!(status["context"]["context_generation"], json!(generation));

    let delivered = fixture.request(json!({
        "id": "deliver-brief",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-cursor",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": "ready",
        "expected_event_cursor": cursor,
        "expected_context_generation": generation
    }));
    assert_eq!(delivered["ok"], true);
    assert!(delivered["event_cursor"].as_u64().unwrap() > cursor);
    assert!(delivered["context_generation"].as_u64().unwrap() > generation);

    let stale = fixture.request(json!({
        "id": "stale-patch",
        "token": fixture.token,
        "op": "patch_board",
        "run_id": "run-cursor",
        "activation_id": "root",
        "patch": {
            "summary": "old"
        },
        "expected_event_cursor": cursor,
        "expected_context_generation": generation
    }));
    assert_eq!(stale["ok"], false);
    assert_eq!(stale["error"]["code"], "conflict");
    assert_eq!(
        stale["error"]["actual_event_cursor"],
        delivered["event_cursor"]
    );
    assert_eq!(
        stale["error"]["actual_context_generation"],
        delivered["context_generation"]
    );
    driver.shutdown();
}

#[test]
fn driver_does_not_publish_runtime_mutation_when_event_persistence_fails() {
    let fixture = DriverFixture::new("driver-rollback");
    let mut driver = fixture.spawn("run-rollback");

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-rollback",
        "flow_lock": locked_flow(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);
    let before = fixture.request(json!({
        "id": "status-before-fault",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-rollback"
    }));
    assert_eq!(before["ok"], true);

    let events_path = fixture.run_events_path("run-rollback");
    fs::remove_file(&events_path).unwrap();
    fs::create_dir(&events_path).unwrap();
    let delivered = fixture.request(json!({
        "id": "deliver-brief",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-rollback",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": {
            "summary": "should not publish"
        }
    }));
    assert_eq!(delivered["ok"], false);

    let after = fixture.request(json!({
        "id": "status-after-fault",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-rollback"
    }));
    assert_eq!(after["ok"], true);
    assert_eq!(after["event_cursor"], before["event_cursor"]);
    assert_eq!(after["context_generation"], before["context_generation"]);
    assert!(after["context"]["artifacts"]["brief"].is_null());
    assert!(after["context"]["activations"]["follow"].is_null());
    driver.shutdown();
}

#[test]
fn driver_snapshot_fault_keeps_committed_event_authority_and_stale_cache() {
    let fixture = DriverFixture::new("driver-snapshot-rollback");
    let snapshot_fault = fixture.root.join("fail-snapshot");
    let snapshot_fault_value = snapshot_fault.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env_values(
        "run-snapshot-rollback",
        &[(
            "HUMANIZE_DRIVER_FAIL_SNAPSHOT_IF_EXISTS",
            &snapshot_fault_value,
        )],
    );

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-snapshot-rollback",
        "flow_lock": locked_flow(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);
    let before = fixture.request(json!({
        "id": "status-before-snapshot-fault",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-snapshot-rollback"
    }));
    let snapshot_path = fixture
        .private_driver_dir("run-snapshot-rollback")
        .join("snapshot.json");
    let snapshot_before_fault = fs::read(&snapshot_path).unwrap();
    fs::write(&snapshot_fault, "fail").unwrap();

    let delivered = fixture.request(json!({
        "id": "deliver-brief-with-snapshot-fault",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-snapshot-rollback",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": {
            "summary": "event log remains authoritative"
        }
    }));
    assert_eq!(delivered["ok"], true, "{delivered}");
    let after = fixture.request(json!({
        "id": "status-after-snapshot-fault",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-snapshot-rollback"
    }));
    assert!(after["event_cursor"].as_u64() > before["event_cursor"].as_u64());
    assert!(after["context_generation"].as_u64() > before["context_generation"].as_u64());
    assert_eq!(
        after["context"]["artifacts"]["brief"]["summary"],
        "event log remains authoritative"
    );
    assert!(
        fs::read_to_string(fixture.run_events_path("run-snapshot-rollback"))
            .unwrap()
            .contains("event log remains authoritative")
    );
    assert_eq!(fs::read(&snapshot_path).unwrap(), snapshot_before_fault);
    driver.crash();

    let mut restarted = fixture.spawn_with_env_values(
        "run-snapshot-rollback",
        &[(
            "HUMANIZE_DRIVER_FAIL_SNAPSHOT_IF_EXISTS",
            &snapshot_fault_value,
        )],
    );
    let replayed = fixture.request(json!({
        "id": "status-after-snapshot-fault-replay",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-snapshot-rollback"
    }));
    assert_eq!(
        replayed["event_cursor"].as_u64(),
        delivered["event_cursor"].as_u64().map(|cursor| cursor + 1)
    );
    assert_eq!(replayed["run_status"], "paused");
    assert_eq!(
        replayed["context"]["artifacts"]["brief"]["summary"],
        "event log remains authoritative"
    );
    restarted.shutdown();
}

#[test]
fn driver_later_event_append_fault_does_not_publish_partial_runtime_transaction() {
    let fixture = DriverFixture::new("driver-second-append-rollback");
    let append_fault = fixture.root.join("fail-second-event");
    let append_fault_value = append_fault.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env_values(
        "run-second-append",
        &[
            ("HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_AT", "2"),
            (
                "HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_IF_EXISTS",
                &append_fault_value,
            ),
        ],
    );

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-second-append",
        "flow_lock": locked_flow(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);
    let before = fixture.request(json!({
        "id": "status-before-second-append-fault",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-second-append"
    }));
    fs::write(&append_fault, "fail").unwrap();

    let failed = fixture.request(json!({
        "id": "deliver-brief-fails-second-append",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-second-append",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": {
            "summary": "second append must not publish"
        }
    }));
    assert_eq!(failed["ok"], false);
    let after = fixture.request(json!({
        "id": "status-after-second-append-fault",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-second-append"
    }));
    assert_eq!(after["event_cursor"], before["event_cursor"]);
    assert_eq!(after["context_generation"], before["context_generation"]);
    assert!(after["context"]["artifacts"]["brief"].is_null());
    assert!(
        !fs::read_to_string(fixture.run_events_path("run-second-append"))
            .unwrap()
            .contains("second append must not publish")
    );

    fs::remove_file(&append_fault).unwrap();
    let retried = fixture.request(json!({
        "id": "deliver-brief-after-second-append-fault",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-second-append",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": {
            "summary": "retry publishes once"
        }
    }));
    assert_eq!(retried["ok"], true);
    driver.crash();

    let mut restarted = fixture.spawn_with_env_values(
        "run-second-append",
        &[
            ("HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_AT", "2"),
            (
                "HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_IF_EXISTS",
                &append_fault_value,
            ),
        ],
    );
    let replayed = fixture.request(json!({
        "id": "status-after-second-append-replay",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-second-append"
    }));
    assert_eq!(
        replayed["event_cursor"].as_u64(),
        retried["event_cursor"].as_u64().map(|cursor| cursor + 1)
    );
    assert_eq!(replayed["run_status"], "paused");
    assert_eq!(
        replayed["context"]["artifacts"]["brief"]["summary"],
        "retry publishes once"
    );
    restarted.shutdown();
}
