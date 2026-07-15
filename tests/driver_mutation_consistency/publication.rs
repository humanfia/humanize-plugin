use super::*;

#[test]
fn restart_rejects_symlinked_publication_directories() {
    use std::os::unix::fs::symlink;

    for directory in ["publication-outbox", "publication-ledger"] {
        let fixture = DriverFixture::new(&format!("driver-publication-{directory}-symlink"));
        let run_id = format!("run-publication-{directory}-symlink");
        let mut driver = fixture.spawn_with_env(&run_id, &[]);
        fixture.bind(&mut driver, &run_id, routed_flow(), None);
        let paused = fixture.request(json!({
            "id": "pause-before-publication-directory-attack",
            "token": fixture.token,
            "op": "pause",
            "run_id": run_id
        }));
        assert_eq!(paused["ok"], true, "{paused}");
        driver.crash();

        let private_directory = fixture.private_driver_dir(&run_id).join(directory);
        let outside = fixture.root.join(format!("outside-{directory}"));
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).unwrap();
        if directory == "publication-ledger" {
            for entry in fs::read_dir(&private_directory).unwrap() {
                let entry = entry.unwrap();
                fs::copy(entry.path(), outside.join(entry.file_name())).unwrap();
            }
        }
        let outside_before = private_directory_files(&outside);
        fs::remove_dir_all(&private_directory).unwrap();
        symlink(&outside, &private_directory).unwrap();

        let stderr = fixture.rejected_restart(&run_id);

        assert_eq!(
            private_directory_files(&outside),
            outside_before,
            "restart changed external files through symlinked {directory}"
        );
        assert!(
            stderr.contains("publication") || stderr.contains("directory"),
            "unexpected restart diagnostic for {directory}: {stderr}"
        );
    }
}

fn private_directory_files(path: &Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
    let mut files = fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

#[test]
fn runtime_append_failure_preserves_previous_snapshot() {
    let fixture = DriverFixture::new("driver-snapshot-prior");
    let append_fault = fixture.root.join("fail-runtime-append");
    let append_fault_value = append_fault.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-snapshot-prior",
        &[
            ("HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_AT", "1"),
            (
                "HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_IF_EXISTS",
                &append_fault_value,
            ),
        ],
    );
    fixture.bind(&mut driver, "run-snapshot-prior", routed_flow(), None);
    let snapshot_path = fixture.snapshot_path("run-snapshot-prior");
    let prior_snapshot = fs::read(&snapshot_path).unwrap();
    fs::write(&append_fault, "fail").unwrap();

    let failed = fixture.request(json!({
        "id": "deliver-with-append-fault",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-snapshot-prior",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": "must not reach the snapshot"
    }));

    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(fs::read(snapshot_path).unwrap(), prior_snapshot);
    driver.crash();
}

#[test]
fn applied_lock_publication_refreshes_snapshot_review_context() {
    let fixture = DriverFixture::new("driver-snapshot-review");
    let mut driver = fixture.spawn_with_env("run-snapshot-review", &[]);
    let bound = fixture.bind(&mut driver, "run-snapshot-review", routed_flow(), None);
    let review_id = bound["flow_lock"]["review_id"].clone();

    let snapshot: Value =
        serde_json::from_slice(&fs::read(fixture.snapshot_path("run-snapshot-review")).unwrap())
            .unwrap();
    assert_eq!(snapshot["flow_revisions"][0]["review"], review_id);
    driver.shutdown();
}

#[test]
fn publication_outbox_persistence_failure_prevents_runtime_mutation() {
    let fixture = DriverFixture::new("driver-publication-outbox-persist");
    let fault = fixture.root.join("fail-publication-outbox");
    let fault_value = fault.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-publication-outbox-persist",
        &[("HUMANIZE_DRIVER_FAIL_OUTBOX_IF_EXISTS", &fault_value)],
    );
    fixture.bind(
        &mut driver,
        "run-publication-outbox-persist",
        routed_flow(),
        None,
    );
    let before = fixture.status("run-publication-outbox-persist");
    let journal_before = fixture.public_journal("run-publication-outbox-persist");
    fs::write(&fault, "fail").unwrap();

    let failed = fixture.request(json!({
        "id": "pause-before-outbox",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-publication-outbox-persist"
    }));

    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "publication_blocked", "{failed}");
    assert_eq!(fixture.status("run-publication-outbox-persist"), before);
    assert_eq!(
        fixture.public_journal("run-publication-outbox-persist"),
        journal_before
    );
    assert_eq!(
        fixture.pending_publication_count("run-publication-outbox-persist"),
        0
    );

    fs::remove_file(fault).unwrap();
    let paused = fixture.request(json!({
        "id": "pause-after-outbox-recovery",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-publication-outbox-persist"
    }));
    assert_eq!(paused["ok"], true, "{paused}");
    assert_eq!(paused["run_status"], "paused", "{paused}");
    driver.shutdown();
}

#[test]
fn publication_sequence_rejects_hardlink_and_fifo_before_runtime_mutation() {
    let fixture = DriverFixture::new("driver-publication-sequence-attacks");
    let mut driver = fixture.spawn_with_env("run-publication-sequence-attacks", &[]);
    fixture.bind(
        &mut driver,
        "run-publication-sequence-attacks",
        routed_flow(),
        None,
    );
    let sequence = fixture
        .private_driver_dir("run-publication-sequence-attacks")
        .join("publication-sequence");
    let alias = sequence.with_extension("hardlink");
    fs::hard_link(&sequence, &alias).unwrap();

    let before_hardlink = fixture.status("run-publication-sequence-attacks");
    let hardlink_rejected = fixture.request(json!({
        "id": "pause-with-hardlinked-publication-sequence",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-publication-sequence-attacks"
    }));
    assert_eq!(hardlink_rejected["ok"], false, "{hardlink_rejected}");
    assert_eq!(
        hardlink_rejected["error"]["code"], "publication_blocked",
        "{hardlink_rejected}"
    );
    assert_eq!(
        fixture.status("run-publication-sequence-attacks"),
        before_hardlink
    );
    fs::remove_file(alias).unwrap();

    let paused = fixture.request(json!({
        "id": "pause-after-hardlink-removal",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-publication-sequence-attacks"
    }));
    assert_eq!(paused["ok"], true, "{paused}");
    fs::remove_file(&sequence).unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(&sequence)
            .status()
            .unwrap()
            .success()
    );

    let fifo_rejected = fixture.request(json!({
        "id": "resume-with-fifo-publication-sequence",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-publication-sequence-attacks"
    }));
    assert_eq!(fifo_rejected["ok"], false, "{fifo_rejected}");
    assert_eq!(
        fifo_rejected["error"]["code"], "publication_blocked",
        "{fifo_rejected}"
    );
    assert_eq!(
        fixture.status("run-publication-sequence-attacks")["run_status"],
        "paused"
    );
    fs::remove_file(sequence).unwrap();

    let resumed = fixture.request(json!({
        "id": "resume-after-fifo-removal",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-publication-sequence-attacks"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(resumed["run_status"], "running", "{resumed}");
    driver.shutdown();
}

#[test]
fn public_event_failure_blocks_mutation_and_restart_replays_outbox() {
    let fixture = DriverFixture::new("driver-publication-event-replay");
    let fault = fixture.root.join("fail-public-event");
    let fault_value = fault.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-publication-event-replay",
        &[("HUMANIZE_DRIVER_FAIL_PUBLIC_EVENT_IF_EXISTS", &fault_value)],
    );
    fixture.bind(
        &mut driver,
        "run-publication-event-replay",
        routed_flow(),
        None,
    );
    let published_before = fixture.published_publication_count("run-publication-event-replay");
    let journal_before = fixture.public_journal("run-publication-event-replay");
    fs::write(&fault, "fail").unwrap();

    let failed = fixture.request(json!({
        "id": "pause-before-publication",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-publication-event-replay"
    }));
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "publication_blocked", "{failed}");
    assert_eq!(
        fixture.status("run-publication-event-replay")["run_status"],
        "paused"
    );
    assert_eq!(
        fixture.public_journal("run-publication-event-replay"),
        journal_before
    );
    assert_eq!(
        fixture.pending_publication_count("run-publication-event-replay"),
        1
    );
    let blocked = fixture.request(json!({
        "id": "resume-while-publication-blocked",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-publication-event-replay"
    }));
    assert_eq!(blocked["error"]["code"], "publication_blocked", "{blocked}");
    driver.crash();

    fs::remove_file(fault).unwrap();
    let mut restarted = fixture.spawn_with_env(
        "run-publication-event-replay",
        &[("HUMANIZE_DRIVER_FAIL_PUBLIC_EVENT_IF_EXISTS", &fault_value)],
    );
    assert_eq!(
        fixture.status("run-publication-event-replay")["run_status"],
        "paused"
    );
    assert_eq!(
        fixture.pending_publication_count("run-publication-event-replay"),
        0
    );
    assert_eq!(
        fixture.published_publication_count("run-publication-event-replay"),
        published_before + 1
    );
    assert!(fixture.public_journal("run-publication-event-replay").len() > journal_before.len());
    restarted.shutdown();
}

#[test]
fn projection_failure_replays_without_duplicate_public_events() {
    let fixture = DriverFixture::new("driver-publication-projection-replay");
    let fault = fixture.root.join("fail-publication-projection");
    let fault_value = fault.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-publication-projection-replay",
        &[("HUMANIZE_DRIVER_FAIL_MANIFEST_IF_EXISTS", &fault_value)],
    );
    fixture.bind(
        &mut driver,
        "run-publication-projection-replay",
        routed_flow(),
        None,
    );
    fs::write(&fault, "fail").unwrap();

    let failed = fixture.request(json!({
        "id": "pause-before-projection",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-publication-projection-replay"
    }));
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "publication_blocked", "{failed}");
    assert_eq!(
        fixture.status("run-publication-projection-replay")["run_status"],
        "paused"
    );
    assert_eq!(
        fixture.public_manifest("run-publication-projection-replay")["status"],
        "running"
    );
    assert_eq!(
        fixture.pending_publication_count("run-publication-projection-replay"),
        1
    );
    let journal_after_event = fixture.public_journal("run-publication-projection-replay");
    driver.crash();

    fs::remove_file(fault).unwrap();
    let mut restarted = fixture.spawn_with_env(
        "run-publication-projection-replay",
        &[("HUMANIZE_DRIVER_FAIL_MANIFEST_IF_EXISTS", &fault_value)],
    );
    assert_eq!(
        fixture.public_journal("run-publication-projection-replay"),
        journal_after_event
    );
    assert_eq!(
        fixture.pending_publication_count("run-publication-projection-replay"),
        0
    );
    assert_eq!(
        fixture.public_manifest("run-publication-projection-replay")["status"],
        "paused"
    );
    restarted.shutdown();
}

#[test]
fn direct_fact_publication_failure_blocks_mutation_and_replays_after_restart() {
    let fixture = DriverFixture::new("driver-direct-fact-publication-replay");
    let fault = fixture.root.join("fail-direct-fact-publication");
    let fault_value = fault.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-direct-fact-publication-replay",
        &[("HUMANIZE_DRIVER_FAIL_PUBLIC_EVENT_IF_EXISTS", &fault_value)],
    );
    fixture.bind(
        &mut driver,
        "run-direct-fact-publication-replay",
        routed_flow(),
        None,
    );
    let journal_before = fixture.public_journal("run-direct-fact-publication-replay");
    fs::write(&fault, "fail").unwrap();

    let failed = fixture.request(json!({
        "id": "record-hook-before-publication",
        "token": fixture.token,
        "op": "record_hook_fact",
        "run_id": "run-direct-fact-publication-replay",
        "session_id": "native-session-a",
        "hook": "compaction_pending",
        "source_native_id": "native-hook-a",
        "payload": {"reason": "test"}
    }));
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "publication_blocked", "{failed}");
    assert_eq!(
        fixture.public_journal("run-direct-fact-publication-replay"),
        journal_before
    );
    assert_eq!(
        fixture.pending_publication_count("run-direct-fact-publication-replay"),
        1
    );

    let blocked = fixture.request(json!({
        "id": "pause-while-direct-fact-publication-blocked",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-direct-fact-publication-replay"
    }));
    assert_eq!(blocked["error"]["code"], "publication_blocked", "{blocked}");
    driver.crash();

    fs::remove_file(fault).unwrap();
    let mut restarted = fixture.spawn_with_env(
        "run-direct-fact-publication-replay",
        &[("HUMANIZE_DRIVER_FAIL_PUBLIC_EVENT_IF_EXISTS", &fault_value)],
    );
    assert_eq!(
        fixture.pending_publication_count("run-direct-fact-publication-replay"),
        0
    );
    let events = fixture.public_journal_events("run-direct-fact-publication-replay");
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "hook.observed")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "context_compaction.started")
            .count(),
        1
    );
    restarted.shutdown();
}

#[test]
fn publication_ack_failure_replays_idempotently_after_restart() {
    let fixture = DriverFixture::new("driver-publication-ack-replay");
    let fault = fixture.root.join("fail-publication-ack");
    let fault_value = fault.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-publication-ack-replay",
        &[("HUMANIZE_DRIVER_FAIL_OUTBOX_ACK_IF_EXISTS", &fault_value)],
    );
    fixture.bind(
        &mut driver,
        "run-publication-ack-replay",
        routed_flow(),
        None,
    );
    let published_before = fixture.published_publication_count("run-publication-ack-replay");
    fs::write(&fault, "fail").unwrap();

    let failed = fixture.request(json!({
        "id": "pause-before-publication-ack",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-publication-ack-replay"
    }));
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "publication_blocked", "{failed}");
    assert_eq!(
        fixture.pending_publication_count("run-publication-ack-replay"),
        1
    );
    let journal_after_publication = fixture.public_journal("run-publication-ack-replay");
    driver.crash();

    fs::remove_file(fault).unwrap();
    let mut restarted = fixture.spawn_with_env(
        "run-publication-ack-replay",
        &[("HUMANIZE_DRIVER_FAIL_OUTBOX_ACK_IF_EXISTS", &fault_value)],
    );
    assert_eq!(
        fixture.public_journal("run-publication-ack-replay"),
        journal_after_publication,
        "restart must not duplicate an already published transaction"
    );
    assert_eq!(
        fixture.pending_publication_count("run-publication-ack-replay"),
        0
    );
    assert_eq!(
        fixture.published_publication_count("run-publication-ack-replay"),
        published_before + 1
    );
    restarted.shutdown();
}
