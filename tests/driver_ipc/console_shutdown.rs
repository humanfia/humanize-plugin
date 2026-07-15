use std::time::Duration;

use serde_json::json;

use super::driver_flows::locked_flow;
use super::support::DriverFixture;

#[test]
fn driver_console_and_ipc_share_the_same_authoritative_state() {
    let fixture = DriverFixture::new("driver-console-ipc");
    let mut driver = fixture.spawn("run-console");

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-console",
        "flow_lock": locked_flow(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);

    driver.console("pause");
    let paused = fixture.request(json!({
        "id": "status-paused",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-console"
    }));
    assert_eq!(paused["run_status"], "paused");

    let resumed = fixture.request(json!({
        "id": "resume-ipc",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-console"
    }));
    assert_eq!(resumed["ok"], true);
    driver.console("status");
    let line = driver
        .read_console_until("run_status=running", Duration::from_secs(2))
        .expect("console status should observe IPC mutation");
    assert!(line.contains("run_status=running"));
    driver.shutdown();
}

#[test]
fn driver_console_detach_keeps_ipc_available_until_explicit_shutdown() {
    let fixture = DriverFixture::new("driver-console-detach");
    let mut driver = fixture.spawn("run-detach");

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-detach",
        "flow_lock": locked_flow(),
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);

    driver.console("detach");
    let status = fixture.request(json!({
        "id": "status-after-detach",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-detach"
    }));
    assert_eq!(status["ok"], true);
    assert_eq!(status["run_status"], "running");

    let shutdown = fixture.request(json!({
        "id": "shutdown",
        "token": fixture.token,
        "op": "shutdown",
        "run_id": "run-detach"
    }));
    assert_eq!(shutdown["ok"], true);
    driver.wait_for_exit(Duration::from_secs(2));
}
