mod support;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::flow::{
    FlowCheckMode, FlowDraft, FlowNode, FlowResource, ResourceKind, flow_lock,
};
use humanize_plugin::input_ledger::{MachineInputLedger, MachineInputRecord, MachineInputStatus};
use humanize_plugin::mcp::McpServer;
use humanize_plugin::run_assets::{
    RunAssetActivationUpdate, RunAssetSink, RunAssetStore, RunAssetTmuxTarget, SessionRelation,
};
use serde_json::{Value, json};

use support::mcp::{RecordingRunner, call_tool, lock_flow, structured, valid_flow};

static NEXT_ASSET_ROOT: AtomicU64 = AtomicU64::new(1);

fn test_temp_dir(name: &str) -> PathBuf {
    let index = NEXT_ASSET_ROOT.fetch_add(1, Ordering::SeqCst);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("{name}-{index}"));
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    path
}

fn read_jsonl(path: impl Into<PathBuf>) -> Vec<Value> {
    let path = path.into();
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{} should be readable: {err}", path.display()))
        .lines()
        .map(|line| serde_json::from_str(line).expect("record line should be JSON"))
        .collect()
}

fn read_record_index(root: impl Into<PathBuf>) -> Value {
    let root = root.into();
    serde_json::from_str(&fs::read_to_string(root.join("records/index.json")).unwrap()).unwrap()
}

fn rpc_error_message(response: &Value) -> String {
    response["error"]["message"]
        .as_str()
        .or_else(|| response["result"]["structuredContent"]["error"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn draft() -> FlowDraft {
    FlowDraft {
        nodes: vec![FlowNode {
            id: "root".to_string(),
            ..FlowNode::default()
        }],
        resources: vec![FlowResource {
            id: "readme.main".to_string(),
            kind: ResourceKind::Readme,
            source: "inline:Exercise durable runtime records.".to_string(),
        }],
        ..FlowDraft::default()
    }
}

fn machine_input_record_with_transaction(
    run_id: &str,
    transaction_id: &str,
    submitted_at_ms: u64,
) -> MachineInputRecord {
    MachineInputRecord {
        run_id: run_id.to_string(),
        activation_id: "root".to_string(),
        pane_id: "%8".to_string(),
        started_at_ms: 1_700_000_000_100,
        submitted_at_ms,
        payload_hash: "fnv1a64:0000000000000001".to_string(),
        normalized_text: "inspect".to_string(),
        submit_key_count: 1,
        transaction_id: transaction_id.to_string(),
        status: MachineInputStatus::Submitted,
    }
}

fn machine_input_record(run_id: &str) -> MachineInputRecord {
    machine_input_record_with_transaction(run_id, "machine-input:abc", 1_700_000_000_120)
}

#[test]
fn auto_sink_uses_humanize_override_and_ignores_sforge_patch_dir() {
    const CHILD: &str = "HUMANIZE_DURABLE_AUTO_SINK_CHILD";
    const EXPECT_ROOT: &str = "HUMANIZE_DURABLE_EXPECT_ROOT";
    if std::env::var_os(CHILD).is_some() {
        let expected_root = PathBuf::from(std::env::var_os(EXPECT_ROOT).unwrap());
        let root = RunAssetStore::runtime_default()
            .run_root("run-auto")
            .unwrap();

        assert!(root.starts_with(&expected_root), "{}", root.display());
        assert!(!root.to_string_lossy().contains(".flowbench"));
        assert!(!root.to_string_lossy().contains("sforge"));
        return;
    }

    let humanize_root = test_temp_dir("humanize-runs-override");
    let sforge_root = test_temp_dir("sforge-patch-dir");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "auto_sink_uses_humanize_override_and_ignores_sforge_patch_dir",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env(EXPECT_ROOT, &humanize_root)
        .env("HUMANIZE_RUNS_DIR", &humanize_root)
        .env("SFORGE_PATCH_DIR", &sforge_root)
        .status()
        .expect("child test should run");

    assert!(status.success());
}

#[test]
fn duplicate_record_append_is_idempotent_by_source_identity() {
    let root = test_temp_dir("durable-records-idempotent");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let mut manifest = store.start_run_manifest("run-idempotent").unwrap();

    store
        .record_qos_intent(
            &mut manifest,
            &humanize_plugin::flow::FlowQosIntent {
                urgency: humanize_plugin::flow::QosUrgency::Interactive,
                completion_target: Some("artifact.done".to_string()),
            },
        )
        .unwrap();
    store
        .record_qos_intent(
            &mut manifest,
            &humanize_plugin::flow::FlowQosIntent {
                urgency: humanize_plugin::flow::QosUrgency::Interactive,
                completion_target: Some("artifact.done".to_string()),
            },
        )
        .unwrap();

    let qos_records = read_jsonl(manifest.root.join("records/qos.jsonl"));
    let index = read_record_index(&manifest.root);
    assert_eq!(qos_records.len(), 1);
    assert_eq!(index["files"]["qos"]["record_count"], 1);
    assert_eq!(index["files"]["qos"]["latest_sequence"], 1);
}

#[test]
fn record_index_rebuilds_by_scanning_streams_after_index_loss() {
    let root = test_temp_dir("durable-records-rebuild-index");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let mut manifest = store.start_run_manifest("run-rebuild-index").unwrap();
    store
        .record_machine_input(
            &mut manifest,
            "node_prompt",
            &machine_input_record("run-rebuild-index"),
        )
        .unwrap();
    fs::remove_file(manifest.root.join("records/index.json")).unwrap();

    let rebuilt = store.rebuild_record_index(&manifest).unwrap();

    assert_eq!(rebuilt["files"]["storage"]["record_count"], 1);
    assert_eq!(rebuilt["files"]["tmux"]["record_count"], 1);
    assert_eq!(rebuilt["files"]["machine_input_ledger"]["record_count"], 1);
    assert!(manifest.root.join("records/index.json").exists());
}

#[test]
fn record_index_rebuild_recovers_torn_final_jsonl_record() {
    let root = test_temp_dir("durable-records-torn-tail");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let mut manifest = store.start_run_manifest("run-torn-tail").unwrap();
    let storage_path = manifest.root.join("records/storage.jsonl");
    let torn_tail = br#"{"record_id":"rec-storage-torn""#;
    fs::OpenOptions::new()
        .append(true)
        .open(&storage_path)
        .unwrap()
        .write_all(torn_tail)
        .unwrap();
    fs::remove_file(manifest.root.join("records/index.json")).unwrap();

    let rebuilt = store.rebuild_record_index(&manifest).unwrap();

    assert_eq!(rebuilt["files"]["storage"]["record_count"], 1);
    assert_eq!(rebuilt["files"]["storage"]["latest_sequence"], 1);
    let storage_payload = fs::read_to_string(&storage_path).unwrap();
    assert!(storage_payload.ends_with('\n'));
    assert!(!storage_payload.contains("rec-storage-torn"));
    let quarantine_dir = manifest.root.join("records/quarantine");
    let quarantine_entries = fs::read_dir(&quarantine_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(quarantine_entries.len(), 1);
    assert_eq!(fs::read(&quarantine_entries[0]).unwrap(), torn_tail);

    store
        .record_qos_intent(
            &mut manifest,
            &humanize_plugin::flow::FlowQosIntent {
                urgency: humanize_plugin::flow::QosUrgency::Interactive,
                completion_target: None,
            },
        )
        .unwrap();
    let qos_records = read_jsonl(manifest.root.join("records/qos.jsonl"));
    assert_eq!(qos_records.len(), 1);
}

#[test]
fn record_index_rebuild_rejects_interior_jsonl_corruption() {
    let root = test_temp_dir("durable-records-interior-corruption");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let manifest = store.start_run_manifest("run-interior-corruption").unwrap();
    let storage_path = manifest.root.join("records/storage.jsonl");
    let original_payload = fs::read(&storage_path).unwrap();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&storage_path)
        .unwrap();
    file.write_all(b"{not-json}\n").unwrap();
    file.write_all(&original_payload).unwrap();
    drop(file);
    fs::remove_file(manifest.root.join("records/index.json")).unwrap();

    let err = store.rebuild_record_index(&manifest).unwrap_err();

    assert!(err.to_string().contains("parse durable record file"));
    assert!(!manifest.root.join("records/quarantine").exists());
}

#[cfg(unix)]
#[test]
fn record_append_rejects_concurrent_writer_lock() {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;

    let root = test_temp_dir("durable-records-writer-lock");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let mut manifest = store.start_run_manifest("run-lock").unwrap();
    let lock_path = manifest.root.join("records/.writer.lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );

    let err = store
        .record_qos_intent(
            &mut manifest,
            &humanize_plugin::flow::FlowQosIntent {
                urgency: humanize_plugin::flow::QosUrgency::Interactive,
                completion_target: None,
            },
        )
        .unwrap_err();

    assert!(err.to_string().contains("record store writer lock"));
}

#[cfg(unix)]
#[test]
fn record_stream_rejects_fifo_without_blocking() {
    use std::os::unix::ffi::OsStrExt;

    const CHILD_ROOT: &str = "HUMANIZE_RECORD_FIFO_ROOT";
    const CHILD_MANIFEST: &str = "HUMANIZE_RECORD_FIFO_MANIFEST";
    if let (Ok(root), Ok(manifest_path)) =
        (std::env::var(CHILD_ROOT), std::env::var(CHILD_MANIFEST))
    {
        let store = RunAssetStore::new(RunAssetSink::Root(PathBuf::from(root)));
        let mut manifest: humanize_plugin::run_assets::RunAssetManifest =
            serde_json::from_str(&fs::read_to_string(manifest_path).unwrap()).unwrap();
        let result = store.record_qos_intent(
            &mut manifest,
            &humanize_plugin::flow::FlowQosIntent {
                urgency: humanize_plugin::flow::QosUrgency::Interactive,
                completion_target: None,
            },
        );
        assert!(result.is_err());
        return;
    }

    let root = test_temp_dir("durable-records-fifo");
    let store = RunAssetStore::new(RunAssetSink::Root(root));
    let manifest = store.start_run_manifest("run-fifo-record").unwrap();
    let qos_path = manifest.root.join("records/qos.jsonl");
    let fifo_c = std::ffi::CString::new(qos_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let manifest_path = manifest.root.join("child-manifest.json");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "record_stream_rejects_fifo_without_blocking",
            "--nocapture",
        ])
        .env(CHILD_ROOT, manifest.root.parent().unwrap())
        .env(CHILD_MANIFEST, &manifest_path)
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() >= std::time::Duration::from_secs(1) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("record stream append blocked on FIFO");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };

    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn machine_input_ledger_rejects_symlink_target() {
    use std::os::unix::fs::symlink;

    let root = test_temp_dir("durable-records-ledger-symlink");
    let store = RunAssetStore::new(RunAssetSink::Root(root));
    let mut manifest = store.start_run_manifest("run-ledger-symlink").unwrap();
    let outside = manifest.root.join("outside.jsonl");
    fs::write(&outside, "").unwrap();
    symlink(&outside, manifest.root.join("machine-inputs.jsonl")).unwrap();

    let err = store
        .record_machine_input(
            &mut manifest,
            "node_prompt",
            &machine_input_record("run-ledger-symlink"),
        )
        .unwrap_err();

    assert!(err.to_string().contains("machine input ledger"));
}

#[test]
fn run_asset_records_index_manifest_and_append_source_native_facts() {
    let root = test_temp_dir("durable-records-store");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let lock = flow_lock(&draft(), FlowCheckMode::Core).unwrap();
    let mut manifest = store.start_run_manifest("run-records").unwrap();

    let record_index = read_record_index(&manifest.root);
    assert_eq!(record_index["root_relative_path"], "records");
    assert_eq!(
        record_index["files"]["storage"]["relative_path"],
        "records/storage.jsonl"
    );
    assert_eq!(record_index["files"]["storage"]["latest_sequence"], 1);
    let lifecycle_records = read_jsonl(manifest.root.join("records/storage.jsonl"));
    assert_eq!(lifecycle_records.len(), 1);
    assert_eq!(
        lifecycle_records[0]["record_id"],
        "rec-storage-00000000000000000001"
    );
    assert_eq!(lifecycle_records[0]["run_id"], "run-records");
    assert_eq!(lifecycle_records[0]["activation_id"], Value::Null);
    assert_eq!(lifecycle_records[0]["source"], "run_assets");
    assert_eq!(
        lifecycle_records[0]["source_native_id"],
        "run_asset_manifest:started"
    );
    assert_eq!(lifecycle_records[0]["wall_time_ms"], 1_700_000_000_123_u64);
    assert_eq!(lifecycle_records[0]["source_sequence"], 1);
    assert_eq!(
        lifecycle_records[0]["fact"]["manifest_path"],
        "manifest.json"
    );

    store
        .persist_flow_revision(&mut manifest, &lock, "hash:abc123", "not_required")
        .unwrap();
    store
        .record_session_association(
            &mut manifest,
            "host-master",
            SessionRelation::Orchestrates,
            None,
        )
        .unwrap();
    store
        .register_expected_activation(&mut manifest, "root", "root", "tmux")
        .unwrap();
    store
        .start_activation_capture(
            &mut manifest,
            RunAssetActivationUpdate {
                activation_id: "root".to_string(),
                node_id: "root".to_string(),
                tmux: RunAssetTmuxTarget {
                    session_id: "host-a".to_string(),
                    window_id: "%7".to_string(),
                    window_name: "run-records".to_string(),
                    pane_id: "%8".to_string(),
                },
                adapter: "tmux".to_string(),
                termination_reason: None,
            },
        )
        .unwrap();
    store
        .mark_activation_capture_acknowledged(&mut manifest, "root")
        .unwrap();
    store
        .complete_activation_capture(&mut manifest, "root", "contract_satisfied", "final")
        .unwrap();
    store
        .record_preservation_error(
            &mut manifest,
            Some("root"),
            Some("test_failure"),
            "final_capture",
            "capture failed after final snapshot",
        )
        .unwrap();

    let record_index = read_record_index(&manifest.root);
    assert_eq!(record_index["files"]["flow"]["latest_sequence"], 2);
    assert_eq!(record_index["files"]["tmux"]["latest_sequence"], 3);
    assert_eq!(record_index["files"]["probe"]["latest_sequence"], 4);
    assert_eq!(record_index["files"]["preservation"]["latest_sequence"], 1);
    assert_eq!(
        record_index["sessions"]["host-a"]["relations"][0]["relation"],
        "executes"
    );
    assert_eq!(
        record_index["sessions"]["host-a"]["relations"][0]["activation_id"],
        "root"
    );
    assert_eq!(
        record_index["sessions"]["host-master"]["relations"][0]["relation"],
        "orchestrates"
    );
    assert_eq!(
        record_index["sessions"]["host-master"]["relations"][0]["activation_id"],
        Value::Null
    );

    let flow_records = read_jsonl(manifest.root.join("records/flow.jsonl"));
    assert_eq!(
        flow_records[0]["source_native_id"],
        "flow_revision:rev-0001:prepared"
    );
    assert_eq!(
        flow_records[0]["fact"]["revision"]["apply_state"],
        "prepared"
    );
    assert_eq!(
        flow_records[1]["source_native_id"],
        "flow_revision:rev-0001:applied"
    );
    assert_eq!(
        flow_records[1]["fact"]["revision"]["apply_state"],
        "applied"
    );

    let probe_records = read_jsonl(manifest.root.join("records/probe.jsonl"));
    assert_eq!(probe_records[0]["fact"]["probe_state"], "planned");
    assert_eq!(probe_records[1]["fact"]["probe_state"], "ready");
    assert_eq!(probe_records[2]["fact"]["probe_state"], "closed");
    assert_eq!(probe_records[3]["fact"]["probe_state"], "suspended");

    let preservation_records = read_jsonl(manifest.root.join("records/preservation.jsonl"));
    assert_eq!(
        preservation_records[0]["fact"]["preservation_error"]["stage"],
        "final_capture"
    );
}

#[test]
fn failed_flow_revision_appends_preservation_record() {
    let root = test_temp_dir("durable-records-flow-failure");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let lock = flow_lock(&draft(), FlowCheckMode::Core).unwrap();
    let mut manifest = store.start_run_manifest("run-flow-failed").unwrap();
    let revision = store
        .prepare_flow_revision(&mut manifest, &lock, "hash:abc123", "not_required")
        .unwrap();

    store
        .mark_flow_revision_failed(&mut manifest, &revision.revision_id, "runtime apply failed")
        .unwrap();

    let record_index = read_record_index(&manifest.root);
    assert_eq!(record_index["files"]["preservation"]["latest_sequence"], 1);
    assert_eq!(record_index["files"]["preservation"]["record_count"], 1);
    let preservation_records = read_jsonl(manifest.root.join("records/preservation.jsonl"));
    assert_eq!(
        preservation_records[0]["source_native_id"],
        "preservation:run:1700000000123"
    );
    assert_eq!(
        preservation_records[0]["fact"]["preservation_error"]["stage"],
        "flow_package"
    );
    assert_eq!(
        preservation_records[0]["fact"]["preservation_error"]["error"],
        "runtime apply failed"
    );
}

#[test]
fn machine_input_ledger_is_indexed_in_manifest_records() {
    let root = test_temp_dir("durable-records-machine-input");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let mut manifest = store.start_run_manifest("run-machine-input").unwrap();
    let record = MachineInputRecord {
        run_id: "run-machine-input".to_string(),
        activation_id: "root".to_string(),
        pane_id: "%8".to_string(),
        started_at_ms: 1_700_000_000_100,
        submitted_at_ms: 1_700_000_000_120,
        payload_hash: "fnv1a64:0000000000000001".to_string(),
        normalized_text: "inspect".to_string(),
        submit_key_count: 1,
        transaction_id: "machine-input:abc".to_string(),
        status: MachineInputStatus::Submitted,
    };

    store
        .record_machine_input(&mut manifest, "node_prompt", &record)
        .unwrap();

    let record_index = read_record_index(&manifest.root);
    assert_eq!(
        record_index["files"]["machine_input_ledger"]["relative_path"],
        "machine-inputs.jsonl"
    );
    assert_eq!(
        record_index["files"]["machine_input_ledger"]["latest_sequence"],
        1
    );
    assert_eq!(
        record_index["files"]["machine_input_ledger"]["record_count"],
        1
    );
}

#[test]
fn machine_input_ledger_recovers_torn_tail_before_append_and_keeps_retry_idempotent() {
    let root = test_temp_dir("durable-records-machine-input-torn-tail");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let mut manifest = store.start_run_manifest("run-machine-input-torn").unwrap();
    let first = machine_input_record_with_transaction(
        "run-machine-input-torn",
        "machine-input:abc",
        1_700_000_000_120,
    );
    let second = machine_input_record_with_transaction(
        "run-machine-input-torn",
        "machine-input:def",
        1_700_000_000_220,
    );
    let ledger_path = manifest.root.join("machine-inputs.jsonl");
    store
        .record_machine_input(&mut manifest, "node_prompt", &first)
        .unwrap();
    let torn_tail = br#"{"transaction_id":"machine-input:torn""#;
    fs::OpenOptions::new()
        .append(true)
        .open(&ledger_path)
        .unwrap()
        .write_all(torn_tail)
        .unwrap();

    store
        .record_machine_input(&mut manifest, "node_prompt", &second)
        .unwrap();
    store
        .record_machine_input(&mut manifest, "node_prompt", &second)
        .unwrap();

    let ledger_payload = fs::read_to_string(&ledger_path).unwrap();
    let ledger_records = ledger_payload
        .lines()
        .map(|line| serde_json::from_str::<MachineInputRecord>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ledger_records.len(), 2);
    assert_eq!(ledger_records[0].transaction_id, "machine-input:abc");
    assert_eq!(ledger_records[1].transaction_id, "machine-input:def");
    assert!(!ledger_payload.contains("machine-input:torn"));
    let record_index = read_record_index(&manifest.root);
    assert_eq!(
        record_index["files"]["machine_input_ledger"]["record_count"],
        2
    );
    assert_eq!(
        record_index["files"]["machine_input_ledger"]["latest_sequence"],
        2
    );
    let quarantine_entries = fs::read_dir(manifest.root.join("records/quarantine"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(quarantine_entries.len(), 1);
    assert_eq!(fs::read(&quarantine_entries[0]).unwrap(), torn_tail);
}

#[test]
fn direct_machine_input_ledger_append_recovers_torn_tail_and_keeps_retry_idempotent() {
    let root = test_temp_dir("durable-records-direct-machine-input-torn-tail");
    fs::create_dir_all(&root).unwrap();
    let ledger_path = root.join("machine-inputs.jsonl");
    let ledger = MachineInputLedger::at_path(&ledger_path);
    let first = machine_input_record_with_transaction(
        "run-direct-machine-input-torn",
        "machine-input:abc",
        1_700_000_000_120,
    );
    let second = machine_input_record_with_transaction(
        "run-direct-machine-input-torn",
        "machine-input:def",
        1_700_000_000_220,
    );
    ledger.append(first).unwrap();
    let torn_tail = br#"{"transaction_id":"machine-input:torn""#;
    fs::OpenOptions::new()
        .append(true)
        .open(&ledger_path)
        .unwrap()
        .write_all(torn_tail)
        .unwrap();

    ledger.append(second.clone()).unwrap();
    ledger.append(second).unwrap();

    let ledger_payload = fs::read_to_string(&ledger_path).unwrap();
    let ledger_records = ledger_payload
        .lines()
        .map(|line| serde_json::from_str::<MachineInputRecord>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ledger_records.len(), 2);
    assert_eq!(ledger_records[0].transaction_id, "machine-input:abc");
    assert_eq!(ledger_records[1].transaction_id, "machine-input:def");
    assert!(!ledger_payload.contains("machine-input:torn"));
    let quarantine_entries = fs::read_dir(root.join("records/quarantine"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(quarantine_entries.len(), 1);
    assert_eq!(fs::read(&quarantine_entries[0]).unwrap(), torn_tail);
}

#[test]
fn machine_input_ledger_rebuild_rejects_interior_corruption() {
    let root = test_temp_dir("durable-records-machine-input-interior-corruption");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let mut manifest = store
        .start_run_manifest("run-machine-input-corruption")
        .unwrap();
    let first = machine_input_record("run-machine-input-corruption");
    let second = machine_input_record_with_transaction(
        "run-machine-input-corruption",
        "machine-input:def",
        1_700_000_000_220,
    );
    store
        .record_machine_input(&mut manifest, "node_prompt", &first)
        .unwrap();
    let ledger_path = manifest.root.join("machine-inputs.jsonl");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&ledger_path)
        .unwrap();
    file.write_all(b"{not-json}\n").unwrap();
    file.write_all(serde_json::to_string(&second).unwrap().as_bytes())
        .unwrap();
    file.write_all(b"\n").unwrap();
    drop(file);
    fs::remove_file(manifest.root.join("records/index.json")).unwrap();

    let err = store.rebuild_record_index(&manifest).unwrap_err();

    assert!(err.to_string().contains("parse machine input ledger"));
    assert!(!manifest.root.join("records/quarantine").exists());
}

#[test]
fn mcp_records_runtime_delivery_hook_and_session_context_generation() {
    let root = test_temp_dir("durable-records-mcp");
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(
        RecordingRunner::default(),
        RunAssetStore::new(RunAssetSink::Root(root)),
    );

    call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-mcp-records",
            "nodes": ["root"],
            "qos": {
                "urgency": "interactive",
                "completion_target": "summary.ready"
            }
        }),
    );
    call_tool(
        &mut server,
        2,
        "deliver_artifact",
        json!({
            "run_id": "run-mcp-records",
            "activation_id": "root",
            "artifact_key": "summary",
            "payload": "ready"
        }),
    );
    let pending = call_tool(
        &mut server,
        3,
        "record_hook_fact",
        json!({
            "run_id": "run-mcp-records",
            "session_id": "host-a",
            "activation_id": "root",
            "hook": "compaction_pending",
            "source_native_id": "hook-1",
            "payload": {
                "reason": "token_budget"
            }
        }),
    );
    let finished = call_tool(
        &mut server,
        4,
        "record_hook_fact",
        json!({
            "run_id": "run-mcp-records",
            "session_id": "host-a",
            "hook": "compaction_finished",
            "source_native_id": "hook-2",
            "payload": {
                "summary_artifact": "artifact.summary"
            }
        }),
    );

    assert_eq!(structured(&pending)["context_generation"], 0);
    assert_eq!(structured(&finished)["context_generation"], 1);

    let context = call_tool(
        &mut server,
        5,
        "get_context",
        json!({
            "run_id": "run-mcp-records"
        }),
    );
    let manifest = &structured(&context)["context"]["run_assets"];
    let run_root = PathBuf::from(manifest["root"].as_str().unwrap());

    assert_eq!(
        manifest["records"]["files"]["runtime"]["latest_sequence"],
        2
    );
    assert_eq!(
        manifest["records"]["files"]["delivery"]["latest_sequence"],
        3
    );
    assert_eq!(manifest["records"]["files"]["delivery"]["record_count"], 1);
    assert_eq!(manifest["records"]["files"]["hook"]["latest_sequence"], 2);
    assert_eq!(manifest["records"]["files"]["qos"]["latest_sequence"], 1);
    assert_eq!(
        manifest["records"]["sessions"]["host-a"]["context_generation"],
        1
    );
    assert_eq!(
        manifest["records"]["sessions"]["host-a"]["relations"][0]["relation"],
        "executes"
    );
    assert_eq!(
        manifest["records"]["sessions"]["host-a"]["relations"][0]["activation_id"],
        "root"
    );

    let runtime_records = read_jsonl(run_root.join("records/runtime.jsonl"));
    assert_eq!(runtime_records[0]["fact"]["event"]["kind"], "run_started");
    assert_eq!(
        runtime_records[1]["fact"]["event"]["kind"],
        "node_activated"
    );

    let delivery_records = read_jsonl(run_root.join("records/delivery.jsonl"));
    assert_eq!(
        delivery_records[0]["fact"]["event"]["kind"],
        "artifact_delivered"
    );
    assert_eq!(
        delivery_records[0]["fact"]["event"]["payload"]["artifact_key"],
        "summary"
    );

    let hook_records = read_jsonl(run_root.join("records/hook.jsonl"));
    assert_eq!(hook_records[0]["fact"]["hook"], "compaction_pending");
    assert_eq!(hook_records[0]["fact"]["context_generation"], 0);
    assert_eq!(hook_records[1]["fact"]["hook"], "compaction_finished");
    assert_eq!(hook_records[1]["fact"]["context_generation"], 1);

    let qos_records = read_jsonl(run_root.join("records/qos.jsonl"));
    assert_eq!(qos_records[0]["source_native_id"], "qos:run_intent");
    assert_eq!(qos_records[0]["fact"]["qos"]["urgency"], "interactive");
    assert_eq!(
        qos_records[0]["fact"]["qos"]["completion_target"],
        "summary.ready"
    );
}

#[test]
fn mcp_rejects_invalid_hook_and_runtime_qos_inputs() {
    let root = test_temp_dir("durable-records-mcp-validation");
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(
        RecordingRunner::default(),
        RunAssetStore::new(RunAssetSink::Root(root)),
    );
    call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-hook-validation",
            "nodes": ["root"]
        }),
    );

    for (id, arguments, expected) in [
        (
            2,
            json!({
                "run_id": "run-hook-validation",
                "session_id": "",
                "hook": "compaction_pending"
            }),
            "session_id must be non-empty",
        ),
        (
            3,
            json!({
                "run_id": "run-hook-validation",
                "session_id": "host-a",
                "hook": "unknown_hook"
            }),
            "hook must be a documented hook name or namespaced extension",
        ),
        (
            4,
            json!({
                "run_id": "run-hook-validation",
                "session_id": "host-a",
                "hook": "vendor.custom_hook",
                "source_native_id": ""
            }),
            "source_native_id must be non-empty",
        ),
        (
            5,
            json!({
                "run_id": "run-hook-validation",
                "session_id": "host-a",
                "activation_id": "missing",
                "hook": "compaction_pending"
            }),
            "activation not found",
        ),
        (
            6,
            json!({
                "run_id": "run-hook-validation",
                "session_id": "host-a",
                "hook": "compaction_pending",
                "payload": "x".repeat(70000)
            }),
            "payload exceeds",
        ),
    ] {
        let response = call_tool(&mut server, id, "record_hook_fact", arguments);
        assert!(
            rpc_error_message(&response).contains(expected),
            "{response}"
        );
    }

    let bad_qos = call_tool(
        &mut server,
        7,
        "start_run",
        json!({
            "run_id": "run-bad-qos",
            "nodes": ["root"],
            "qos": {
                "urgency": "interactive",
                "completion_target": ""
            }
        }),
    );
    assert!(rpc_error_message(&bad_qos).contains("qos.completion_target must be non-empty"));
}

#[test]
fn mcp_record_hook_unknown_run_does_not_create_run_assets() {
    let root = test_temp_dir("durable-records-unknown-hook-run");
    let store = RunAssetStore::new(RunAssetSink::Root(root));
    let unknown_run_root = store.run_root("run-missing-hook").unwrap();
    let mut server =
        McpServer::with_tmux_runner_and_run_asset_store(RecordingRunner::default(), store);

    let response = call_tool(
        &mut server,
        1,
        "record_hook_fact",
        json!({
            "run_id": "run-missing-hook",
            "session_id": "host-a",
            "hook": "compaction_pending",
            "source_native_id": "hook-missing-run"
        }),
    );

    assert!(rpc_error_message(&response).contains("run not found"));
    assert!(!unknown_run_root.exists(), "{}", unknown_run_root.display());
}

#[test]
fn mcp_records_explicit_fanout_and_route_topology_decisions() {
    let root = test_temp_dir("durable-records-topology");
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(
        RecordingRunner::default(),
        RunAssetStore::new(RunAssetSink::Root(root)),
    );

    call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-explicit-fanout",
            "nodes": ["root"]
        }),
    );
    call_tool(
        &mut server,
        2,
        "deliver_artifact",
        json!({
            "run_id": "run-explicit-fanout",
            "activation_id": "root",
            "artifact_key": "items",
            "payload": "alpha\nbeta"
        }),
    );
    call_tool(
        &mut server,
        3,
        "fanout_from_artifact",
        json!({
            "run_id": "run-explicit-fanout",
            "node_id": "process",
            "artifact_key": "items",
            "for_each": "items"
        }),
    );
    let explicit_context = call_tool(
        &mut server,
        4,
        "get_context",
        json!({
            "run_id": "run-explicit-fanout"
        }),
    );
    let explicit_manifest = &structured(&explicit_context)["context"]["run_assets"];
    let explicit_root = PathBuf::from(explicit_manifest["root"].as_str().unwrap());
    assert_eq!(
        explicit_manifest["records"]["files"]["topology"]["latest_sequence"],
        1
    );
    let fanout_records = read_jsonl(explicit_root.join("records/topology.jsonl"));
    assert_eq!(
        fanout_records[0]["source_native_id"],
        "fanout:process:items"
    );
    assert_eq!(
        fanout_records[0]["fact"]["decision"],
        "fanout_from_artifact"
    );
    assert_eq!(fanout_records[0]["fact"]["node_id"], "process");
    assert_eq!(fanout_records[0]["fact"]["source_artifact"]["key"], "items");
    assert_eq!(
        fanout_records[0]["fact"]["planned_activation_ids"],
        json!(["process:items/0", "process:items/1"])
    );
    assert_eq!(
        fanout_records[0]["fact"]["applied_activation_ids"],
        json!(["process:items/0", "process:items/1"])
    );
    assert_eq!(
        fanout_records[0]["causal_id"],
        fanout_records[0]["fact"]["source_artifact"]["artifact_id"]
    );

    let (lock_id, content_hash) = lock_flow(&mut server, 5, valid_flow());
    let route_started = call_tool(
        &mut server,
        6,
        "run_flow",
        json!({
            "run_id": "run-route-decision",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "review_required": false
        }),
    );
    assert_eq!(structured(&route_started)["ok"], true);
    call_tool(
        &mut server,
        7,
        "deliver_artifact",
        json!({
            "run_id": "run-route-decision",
            "activation_id": "root",
            "artifact_key": "ready",
            "payload": "true"
        }),
    );
    call_tool(
        &mut server,
        8,
        "observe_stop",
        json!({
            "run_id": "run-route-decision",
            "activation_id": "root",
            "reason": "root done"
        }),
    );
    let route_context = call_tool(
        &mut server,
        9,
        "get_context",
        json!({
            "run_id": "run-route-decision"
        }),
    );
    let route_manifest = &structured(&route_context)["context"]["run_assets"];
    let route_root = PathBuf::from(route_manifest["root"].as_str().unwrap());
    assert_eq!(
        route_manifest["records"]["files"]["topology"]["latest_sequence"],
        1
    );
    let route_records = read_jsonl(route_root.join("records/topology.jsonl"));
    assert_eq!(route_records[0]["source_native_id"], "route:route-0");
    assert_eq!(route_records[0]["fact"]["decision"], "route_applied");
    assert_eq!(route_records[0]["fact"]["route"]["route_index"], 0);
    assert_eq!(route_records[0]["fact"]["route"]["route_id"], "route-0");
    assert_eq!(
        route_records[0]["fact"]["route"]["predicate"],
        "exists(artifact.ready)"
    );
    assert_eq!(
        route_records[0]["fact"]["planned_activation_ids"],
        json!(["finish"])
    );
    assert_eq!(
        route_records[0]["fact"]["applied_activation_ids"],
        json!(["finish"])
    );
    assert_eq!(route_records[0]["fact"]["source_artifact"]["key"], "ready");
    assert_eq!(
        route_records[0]["causal_id"],
        route_records[0]["fact"]["source_artifact"]["artifact_id"]
    );
}
