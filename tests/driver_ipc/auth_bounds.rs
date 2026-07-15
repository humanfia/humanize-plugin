use std::fs;
use std::os::unix::net::UnixStream;
use std::process::Command;

use serde_json::json;

use super::support::DriverFixture;

#[test]
fn driver_rejects_unauthorized_ipc_without_mutating_run_state() {
    let fixture = DriverFixture::new("driver-unauthorized");
    let mut driver = fixture.spawn("run-auth");

    let response = fixture.request(json!({
        "id": "bad-token",
        "token": "wrong",
        "op": "status",
        "run_id": "run-auth"
    }));

    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "unauthorized");
    assert!(!fixture.run_events_path("run-auth").exists());
    driver.shutdown();
}

#[test]
fn driver_reports_malformed_and_stale_socket_errors() {
    let fixture = DriverFixture::new("driver-errors");
    let mut driver = fixture.spawn("run-errors");

    let malformed = fixture.raw_request("{not-json}\n");
    assert_eq!(malformed["ok"], false);
    assert_eq!(malformed["error"]["code"], "malformed_request");
    driver.shutdown();

    let socket_path = fixture.socket_path("run-errors");
    assert!(
        UnixStream::connect(&socket_path).is_err(),
        "shutdown should not leave a live socket"
    );
    fs::write(&socket_path, "").unwrap();
    assert!(
        UnixStream::connect(&socket_path).is_err(),
        "stale socket file should not accept connections"
    );
}

#[test]
fn driver_refuses_to_remove_non_socket_runtime_path() {
    let fixture = DriverFixture::new("driver-stale-non-socket");
    let socket_path = fixture.socket_path("run-stale-path");
    fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
    fs::write(&socket_path, "sentinel").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-driver"))
        .arg("--run-id")
        .arg("run-stale-path")
        .arg("--runs-root")
        .arg(fixture.root.join("runs"))
        .arg("--runtime-root")
        .arg(&fixture.runtime_root)
        .arg("--auth-token")
        .arg(fixture.token)
        .env(
            "HUMANIZE_STATE_ROOT",
            fixture.runtime_root.parent().unwrap(),
        )
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(&socket_path).unwrap(), "sentinel");
}
