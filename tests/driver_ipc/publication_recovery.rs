use std::fs;

use humanize_plugin::input_ledger::machine_input_payload_hash;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use super::driver_flows::reviewed_lock_package;
use super::participant_lifecycle::{
    bind_and_activate_agent, participant_bind_request, participant_binding,
};
use super::support::DriverFixture;

#[test]
fn restart_publishes_committed_flow_revision_without_bind_retry() {
    let fixture = DriverFixture::new("publication-recovery-flow-revision");
    let fault = fixture.root.join("fail-flow-revision-index");
    fs::write(&fault, "fail").unwrap();
    let fault_value = fault.to_string_lossy().to_string();
    let driver_env = [
        (
            "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS",
            fault_value.as_str(),
        ),
        (
            "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
            "flow_revision_available",
        ),
    ];
    let mut driver = fixture.spawn_with_env_values("run-publication-flow", &driver_env);
    let package = reviewed_lock_package();

    let failed = fixture.request(json!({
        "id": "bind-with-revision-index-fault",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-publication-flow",
        "flow_lock": package,
        "review": {"review_id": "review-approved", "status": "approved"}
    }));
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "persistence_failed", "{failed}");
    driver.crash();

    fs::remove_file(&fault).unwrap();
    let mut restarted = fixture.spawn_with_env_values("run-publication-flow", &driver_env);
    let status = fixture.request(json!({
        "id": "status-after-restart",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-publication-flow"
    }));
    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(
        public_event_count(&fixture, "run-publication-flow", |event| {
            event["kind"] == "flow_revision.prepared"
        }),
        1
    );
    assert_eq!(
        public_event_count(&fixture, "run-publication-flow", |event| {
            event["kind"] == "flow_revision.applied"
        }),
        1
    );
    restarted.shutdown();
}

#[test]
fn restart_publishes_native_binding_facts_from_private_authority() {
    let fixture = DriverFixture::new("publication-recovery-native-binding");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-publication-binding")
        .unwrap();
    let fake_tmux = fixture.fake_tmux_without_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let fault = fixture.root.join("fail-binding-publication-outbox");
    let fault_value = fault.to_string_lossy().to_string();
    let driver_env = [
        ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
        ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "25"),
        (
            "HUMANIZE_DRIVER_FAIL_OUTBOX_IF_EXISTS",
            fault_value.as_str(),
        ),
    ];
    let mut driver = fixture.spawn_with_env_values("run-publication-binding", &driver_env);
    bind_and_activate_agent(&fixture, "run-publication-binding");
    let binding = participant_binding(&fixture, "run-publication-binding");
    fs::write(&fault, "fail").unwrap();

    let failed = fixture.request(participant_bind_request(
        &binding,
        fixture.token,
        "run-publication-binding",
    ));
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "publication_blocked", "{failed}");
    assert_eq!(
        fs::read_to_string(fixture.driver_events_path("run-publication-binding"))
            .unwrap()
            .matches("\"kind\":\"participant_bound\"")
            .count(),
        1
    );
    driver.crash();

    fs::remove_file(&fault).unwrap();
    let mut restarted = fixture.spawn_with_env_values("run-publication-binding", &driver_env);
    let status = fixture.request(json!({
        "id": "status-after-binding-restart",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-publication-binding"
    }));
    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(
        public_event_count(&fixture, "run-publication-binding", |event| {
            event["kind"] == "agent_session.started"
        }),
        1
    );
    assert_eq!(
        public_event_count(&fixture, "run-publication-binding", |event| {
            event["kind"] == "agent_session.bound"
        }),
        1
    );
    assert_eq!(
        public_event_count(&fixture, "run-publication-binding", |event| {
            event["kind"] == "hook.observed"
                && event["data"]["payload"]["hook_kind"] == "agent_ready"
        }),
        1
    );
    restarted.shutdown();
}

#[test]
fn restart_publishes_submitted_private_machine_input_once() {
    let fixture = DriverFixture::new("publication-recovery-machine-input");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-publication-input")
        .unwrap();
    let fake_tmux = fixture.fake_tmux_without_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let fault = fixture.root.join("fail-machine-input-publication-outbox");
    let fault_value = fault.to_string_lossy().to_string();
    let driver_env = [
        ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
        ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "25"),
        (
            "HUMANIZE_DRIVER_FAIL_OUTBOX_IF_EXISTS",
            fault_value.as_str(),
        ),
    ];
    let mut driver = fixture.spawn_with_env_values("run-publication-input", &driver_env);
    bind_and_activate_agent(&fixture, "run-publication-input");
    let binding = participant_binding(&fixture, "run-publication-input");
    let bound = fixture.request(participant_bind_request(
        &binding,
        fixture.token,
        "run-publication-input",
    ));
    assert_eq!(bound["ok"], true, "{bound}");
    let message = "publication recovery message";
    let payload_hash = machine_input_payload_hash(message);
    fs::write(&fault, "fail").unwrap();

    let ambiguous = fixture.request(json!({
        "id": "message-with-publication-fault",
        "token": fixture.token,
        "op": "send_message",
        "run_id": "run-publication-input",
        "activation_id": "manual",
        "message_id": "publication-recovery-message",
        "text": message
    }));
    assert_eq!(ambiguous["ok"], false, "{ambiguous}");
    assert_eq!(ambiguous["error"]["code"], "ambiguous_delivery");
    assert_eq!(
        machine_input_event_count(&fixture, "run-publication-input", &payload_hash),
        0
    );
    driver.crash();

    fs::remove_file(&fault).unwrap();
    let mut restarted = fixture.spawn_with_env_values("run-publication-input", &driver_env);
    assert_eq!(
        machine_input_event_count(&fixture, "run-publication-input", &payload_hash),
        1
    );
    let repeated = fixture.request(json!({
        "id": "repeat-message-after-publication-recovery",
        "token": fixture.token,
        "op": "send_message",
        "run_id": "run-publication-input",
        "activation_id": "manual",
        "message_id": "publication-recovery-message",
        "text": message
    }));
    assert_eq!(
        repeated["error"]["code"], "ambiguous_delivery",
        "{repeated}"
    );
    assert_eq!(
        machine_input_event_count(&fixture, "run-publication-input", &payload_hash),
        1
    );
    restarted.shutdown();
}

#[test]
fn restart_discards_a_valid_interrupted_publication_temp_file() {
    let fixture = DriverFixture::new("publication-recovery-interrupted-temp");
    let mut driver = fixture.spawn("run-publication-interrupted-temp");
    let package = reviewed_lock_package();
    let bound = fixture.request(json!({
        "id": "bind-before-interrupted-temp",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-publication-interrupted-temp",
        "flow_lock": package,
        "review": {"review_id": "review-approved", "status": "approved"}
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    driver.crash();

    let driver_dir = fixture.private_driver_dir("run-publication-interrupted-temp");
    let ledger_dir = driver_dir.join("publication-ledger");
    let transaction = fs::read_dir(&ledger_dir).unwrap().next().unwrap().unwrap();
    let transaction_name = transaction.file_name();
    let outbox_dir = driver_dir.join("publication-outbox");
    fs::create_dir_all(&outbox_dir).unwrap();
    let interrupted = outbox_dir.join(format!(
        ".{}.tmp-999999-1",
        transaction_name.to_string_lossy()
    ));
    fs::copy(transaction.path(), &interrupted).unwrap();

    let mut restarted = fixture.spawn("run-publication-interrupted-temp");
    assert!(!interrupted.exists());
    restarted.shutdown();
}

fn public_event_count(
    fixture: &DriverFixture,
    run_id: &str,
    predicate: impl Fn(&Value) -> bool,
) -> usize {
    fs::read_to_string(fixture.run_root(run_id).join("records/events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(predicate)
        .count()
}

fn machine_input_event_count(fixture: &DriverFixture, run_id: &str, payload_hash: &str) -> usize {
    public_event_count(fixture, run_id, |event| {
        event["kind"] == "machine_input.delivered"
            && event["data"]["payload"]["content"]["sha256"] == payload_hash
    })
}
