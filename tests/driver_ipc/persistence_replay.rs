use std::fs;
use std::io::Write;
use std::time::Duration;

use serde_json::json;

use super::driver_flows::{locked_flow, routed_locked_flow};
use super::support::DriverFixture;

#[test]
fn driver_replay_restores_bound_run_artifacts_revisions_and_activations() {
    let fixture = DriverFixture::new("driver-replay");
    let mut driver = fixture.spawn("run-replay");

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-replay",
        "flow_lock": locked_flow(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);
    assert_eq!(bound["run_status"], "running");
    assert_eq!(bound["activation_ids"], json!(["root"]));

    let delivered = fixture.request(json!({
        "id": "deliver-brief",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-replay",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": {
            "summary": "ready"
        }
    }));
    assert_eq!(delivered["ok"], true);

    let revised = fixture.request(json!({
        "id": "apply-revision",
        "token": fixture.token,
        "op": "apply_flow_revision",
        "run_id": "run-replay",
        "flow_lock": routed_locked_flow()
    }));
    assert_eq!(revised["ok"], true);
    let patched = fixture.request(json!({
        "id": "patch-board",
        "token": fixture.token,
        "op": "patch_board",
        "run_id": "run-replay",
        "activation_id": "root",
        "patch": {
            "summary": "board-ready"
        }
    }));
    assert_eq!(patched["ok"], true);
    let effect = fixture.request(json!({
        "id": "record-effect",
        "token": fixture.token,
        "op": "record_effect",
        "run_id": "run-replay",
        "activation_id": "root",
        "effect_key": "notified",
        "payload": {
            "ok": true
        }
    }));
    assert_eq!(effect["ok"], true);
    driver.crash();

    let mut restarted = fixture.spawn("run-replay");
    let status = fixture.request(json!({
        "id": "status-after-replay",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-replay"
    }));

    assert_eq!(status["ok"], true);
    assert_eq!(status["run_status"], "paused");
    assert_eq!(status["context"]["artifacts"]["brief"]["summary"], "ready");
    assert_eq!(status["context"]["board"]["summary"], "board-ready");
    assert_eq!(status["context"]["effects"]["root/notified"]["ok"], true);
    assert_eq!(
        status["context"]["flow_revisions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        status["context"]["flow_revisions"][0]["revision_id"]
            .as_str()
            .unwrap()
            .starts_with("flow-lock-application:")
    );
    let revision_sequences = status["context"]["flow_revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|revision| revision["event_sequence"].as_u64().unwrap())
        .collect::<Vec<_>>();
    let revisions_are_ordered = revision_sequences
        .windows(2)
        .all(|window| window[0] < window[1]);
    assert!(revisions_are_ordered, "{revision_sequences:?}");
    assert_eq!(
        status["context"]["activations"]["root"]["status"],
        "running"
    );
    assert_eq!(
        status["context"]["activations"]["follow"]["status"],
        "running"
    );
    restarted.shutdown();
}

#[test]
fn driver_replay_ignores_torn_final_runtime_and_driver_records() {
    let fixture = DriverFixture::new("driver-torn-tail-replay");
    let mut driver = fixture.spawn("run-torn-tail");

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-torn-tail",
        "flow_lock": locked_flow(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);
    let delivered = fixture.request(json!({
        "id": "deliver-brief",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-torn-tail",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": {
            "summary": "durable"
        }
    }));
    assert_eq!(delivered["ok"], true);
    driver.shutdown();

    fs::OpenOptions::new()
        .append(true)
        .open(fixture.run_events_path("run-torn-tail"))
        .unwrap()
        .write_all(br#"{"event":"torn""#)
        .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(fixture.driver_events_path("run-torn-tail"))
        .unwrap()
        .write_all(br#"{"seq":999,"kind":"torn""#)
        .unwrap();

    let mut restarted = fixture.spawn("run-torn-tail");
    let status = fixture.request(json!({
        "id": "status-after-torn-tail",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-torn-tail"
    }));

    assert_eq!(status["ok"], true);
    assert_eq!(
        status["context"]["artifacts"]["brief"]["summary"],
        "durable"
    );
    assert_eq!(
        status["context"]["flow_revisions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    restarted.shutdown();
}

#[test]
fn driver_recovery_truncates_torn_tail_before_later_append_and_restart() {
    let fixture = DriverFixture::new("driver-torn-tail-append");
    let mut driver = fixture.spawn("run-torn-append");

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-torn-append",
        "flow_lock": locked_flow(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);
    driver.shutdown();

    fs::OpenOptions::new()
        .append(true)
        .open(fixture.run_events_path("run-torn-append"))
        .unwrap()
        .write_all(br#"{"event":"torn""#)
        .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(fixture.driver_events_path("run-torn-append"))
        .unwrap()
        .write_all(br#"{"seq":999,"kind":"torn""#)
        .unwrap();

    let mut recovered = fixture.spawn("run-torn-append");
    let patched = fixture.request(json!({
        "id": "patch-board-after-recovery",
        "token": fixture.token,
        "op": "patch_board",
        "run_id": "run-torn-append",
        "activation_id": "root",
        "patch": {
            "after_recovery": "ok"
        }
    }));
    assert_eq!(patched["ok"], true);
    recovered.shutdown();

    let mut restarted = fixture.spawn("run-torn-append");
    let status = fixture.request(json!({
        "id": "status-after-second-restart",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-torn-append"
    }));
    assert_eq!(status["ok"], true);
    assert_eq!(status["context"]["board"]["after_recovery"], "ok");
    restarted.shutdown();
}

#[test]
fn driver_replay_rejects_interior_driver_event_corruption() {
    let fixture = DriverFixture::new("driver-interior-corruption");
    let mut driver = fixture.spawn("run-corrupt-events");

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-corrupt-events",
        "flow_lock": locked_flow(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);
    driver.shutdown();

    fs::OpenOptions::new()
        .append(true)
        .open(fixture.driver_events_path("run-corrupt-events"))
        .unwrap()
        .write_all(b"not-json\n")
        .unwrap();

    let output = fixture.spawn_until_exit("run-corrupt-events", Duration::from_secs(2));
    assert!(!output.status.success());
}

#[test]
fn driver_replay_restores_review_provenance_in_status_context() {
    let fixture = DriverFixture::new("driver-review-provenance");
    let mut driver = fixture.spawn("run-review");

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-review",
        "flow_lock": locked_flow()
    }));
    assert_eq!(bound["ok"], true);
    let flow_lock_id = bound["flow_lock_id"].clone();
    let review_id = bound["flow_lock"]["review_id"].clone();
    driver.shutdown();

    let mut restarted = fixture.spawn("run-review");
    let status = fixture.request(json!({
        "id": "status-after-replay",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-review"
    }));

    let revision = &status["context"]["flow_revisions"][0];
    assert_eq!(revision["flow_lock_id"], flow_lock_id);
    assert_eq!(revision["review"], review_id);
    restarted.shutdown();
}
