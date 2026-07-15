mod support;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::flow::{FlowQosIntent, QosUrgency};
use humanize_plugin::input_ledger::{MachineInputLedger, MachineInputRecord, MachineInputStatus};
use humanize_plugin::mcp::McpServer;
use humanize_plugin::run_assets::{
    HookFactDetail, HookFactInput, RunAssetActivationUpdate, RunAssetManifest, RunAssetSink,
    RunAssetStore, RunAssetTmuxTarget, SessionRelation, TopologyDecisionInput,
    TopologyDecisionSource,
};
use serde_json::{Value, json};

use support::mcp::{RecordingRunner, call_tool};

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

fn journal_events(manifest: &RunAssetManifest) -> Vec<Value> {
    fs::read_to_string(manifest.root.join("records/events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn events_of_kind(manifest: &RunAssetManifest, kind: &str) -> Vec<Value> {
    journal_events(manifest)
        .into_iter()
        .filter(|event| event["kind"] == kind)
        .collect()
}

fn qos() -> FlowQosIntent {
    FlowQosIntent {
        urgency: QosUrgency::Interactive,
        completion_target: Some("artifact.done".to_string()),
    }
}

fn machine_input_record(run_id: &str, transaction_id: &str) -> MachineInputRecord {
    MachineInputRecord {
        run_id: run_id.to_string(),
        activation_id: "root".to_string(),
        pane_id: "%8".to_string(),
        allocation_generation: 0,
        started_at_ms: 1_700_000_000_100,
        submitted_at_ms: 1_700_000_000_120,
        payload_hash: format!("sha256:{}", "a".repeat(64)),
        normalized_text: "inspect".to_string(),
        submit_key_count: 1,
        transaction_id: transaction_id.to_string(),
        status: MachineInputStatus::Submitted,
    }
}

fn start_activation(store: &RunAssetStore, manifest: &mut RunAssetManifest) {
    store
        .start_activation_capture(
            manifest,
            RunAssetActivationUpdate {
                activation_id: "root".into(),
                node_id: "root".into(),
                adapter: "tmux".into(),
                tmux: RunAssetTmuxTarget {
                    session_id: "host-a".into(),
                    window_id: "%7".into(),
                    window_name: "flow-a".into(),
                    pane_id: "%8".into(),
                    allocation_generation: 0,
                },
                termination_reason: None,
            },
        )
        .unwrap();
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

    let humanize_root = std::env::temp_dir().join(format!(
        "humanize-runs-override-{}-{}",
        std::process::id(),
        NEXT_ASSET_ROOT.fetch_add(1, Ordering::SeqCst)
    ));
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "auto_sink_uses_humanize_override_and_ignores_sforge_patch_dir",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env(EXPECT_ROOT, &humanize_root)
        .env("HUMANIZE_RUNS_DIR", &humanize_root)
        .env("SFORGE_PATCH_DIR", test_temp_dir("sforge-patch-dir"))
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn typed_journal_append_is_idempotent_and_has_no_split_streams() {
    let store = RunAssetStore::new_with_fixed_clock(
        RunAssetSink::Root(test_temp_dir("durable-idempotent")),
        1_700_000_000_123,
    );
    let mut manifest = store.start_run_manifest("run-idempotent").unwrap();
    store.record_qos_intent(&mut manifest, &qos()).unwrap();
    store.record_qos_intent(&mut manifest, &qos()).unwrap();

    let events = events_of_kind(&manifest, "qos.observed");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["data"]["source"], "run_assets");
    assert_eq!(events[0]["data"]["payload"]["state"], "observed");
    assert_eq!(events[0]["data"]["payload"]["urgency"], "interactive");
    for path in [
        "records/index.json",
        "records/qos.jsonl",
        "records/runtime.jsonl",
        "records/topology.jsonl",
        "machine-inputs.jsonl",
    ] {
        assert!(!manifest.root.join(path).exists(), "{path}");
    }
}

#[test]
fn journal_rebuild_recovers_torn_tail_and_rejects_interior_corruption() {
    let store = RunAssetStore::new_with_fixed_clock(
        RunAssetSink::Root(test_temp_dir("durable-journal-recovery")),
        1_700_000_000_123,
    );
    let mut manifest = store.start_run_manifest("run-journal-recovery").unwrap();
    store.record_qos_intent(&mut manifest, &qos()).unwrap();
    let events_path = manifest.root.join("records/events.jsonl");
    let torn = br#"{"seq":999"#;
    fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .unwrap()
        .write_all(torn)
        .unwrap();

    let rebuilt = store.rebuild_record_index(&manifest).unwrap();
    assert_eq!(rebuilt["files"]["events"]["record_count"], 1);
    assert!(fs::read_to_string(&events_path).unwrap().ends_with('\n'));
    let quarantine = fs::read_dir(manifest.root.join("records/quarantine"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(quarantine.len(), 1);
    assert_eq!(fs::read(&quarantine[0]).unwrap(), torn);

    fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .unwrap()
        .write_all(b"{not-json}\n")
        .unwrap();
    let error = store.rebuild_record_index(&manifest).unwrap_err();
    assert!(error.to_string().contains("parse public journal"));
}

#[cfg(unix)]
#[test]
fn journal_append_rejects_fifo_without_blocking() {
    use std::os::unix::ffi::OsStrExt;

    const CHILD_ROOT: &str = "HUMANIZE_RECORD_FIFO_ROOT";
    const CHILD_MANIFEST: &str = "HUMANIZE_RECORD_FIFO_MANIFEST";
    if let (Ok(root), Ok(manifest_path)) =
        (std::env::var(CHILD_ROOT), std::env::var(CHILD_MANIFEST))
    {
        let store = RunAssetStore::new(RunAssetSink::Root(PathBuf::from(root)));
        let mut manifest: RunAssetManifest =
            serde_json::from_slice(&fs::read(manifest_path).unwrap()).unwrap();
        assert!(store.record_qos_intent(&mut manifest, &qos()).is_err());
        return;
    }

    let root = test_temp_dir("durable-journal-fifo");
    let store = RunAssetStore::new(RunAssetSink::Root(root));
    let mut manifest = store.start_run_manifest("run-fifo").unwrap();
    store.record_qos_intent(&mut manifest, &qos()).unwrap();
    let events_path = manifest.root.join("records/events.jsonl");
    fs::remove_file(&events_path).unwrap();
    let fifo = std::ffi::CString::new(events_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let manifest_path = manifest.root.join("private-test-manifest.json");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "journal_append_rejects_fifo_without_blocking",
            "--nocapture",
        ])
        .env(CHILD_ROOT, manifest.root.parent().unwrap())
        .env(CHILD_MANIFEST, &manifest_path)
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if started.elapsed() >= std::time::Duration::from_secs(1) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("journal append blocked on FIFO");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn private_machine_input_ledger_rejects_symlink_hardlink_and_public_mode() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    for case in ["symlink", "hardlink", "public-mode"] {
        let root = test_temp_dir(&format!("durable-ledger-{case}"));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let ledger = root.join("machine-inputs.jsonl");
        let outside = root.join("outside.jsonl");
        fs::write(&outside, b"").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
        match case {
            "symlink" => symlink(&outside, &ledger).unwrap(),
            "hardlink" => fs::hard_link(&outside, &ledger).unwrap(),
            "public-mode" => {
                fs::write(&ledger, b"").unwrap();
                fs::set_permissions(&ledger, fs::Permissions::from_mode(0o644)).unwrap();
            }
            _ => unreachable!(),
        }
        let error = MachineInputLedger::at_path(&ledger)
            .append(machine_input_record("run-ledger", "machine-input:one"))
            .unwrap_err();
        assert!(error.to_string().contains("machine input ledger"));
        assert_eq!(fs::read(&outside).unwrap(), b"");
    }
}

#[test]
fn private_machine_input_ledger_recovers_torn_tail_idempotently() {
    use std::os::unix::fs::PermissionsExt;

    let root = test_temp_dir("durable-ledger-torn");
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("machine-inputs.jsonl");
    let ledger = MachineInputLedger::at_path(&path);
    ledger
        .append(machine_input_record("run-ledger", "machine-input:one"))
        .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(br#"{"transaction_id":"torn""#)
        .unwrap();
    let second = machine_input_record("run-ledger", "machine-input:two");
    ledger.append(second.clone()).unwrap();
    ledger.append(second).unwrap();
    let records = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<MachineInputRecord>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
}

#[test]
fn machine_input_event_is_typed_without_prompt_or_public_ledger() {
    let store = RunAssetStore::new_with_fixed_clock(
        RunAssetSink::Root(test_temp_dir("durable-machine-input")),
        1_700_000_000_123,
    );
    let mut manifest = store.start_run_manifest("run-machine-input").unwrap();
    start_activation(&store, &mut manifest);
    let record = machine_input_record("run-machine-input", "machine-input:one");
    store
        .record_machine_input(&mut manifest, "node_prompt", &record)
        .unwrap();
    store
        .record_machine_input(&mut manifest, "node_prompt", &record)
        .unwrap();

    let events = events_of_kind(&manifest, "machine_input.delivered");
    assert_eq!(events.len(), 1);
    let payload = &events[0]["data"]["payload"];
    assert_eq!(payload["status"], "node_prompt:submitted");
    assert_eq!(payload["content"]["sha256"], record.payload_hash);
    assert_eq!(payload["content"]["length"], 7);
    assert!(payload["content"].get("path").is_none());
    let public_bytes = fs::read(manifest.root.join("records/events.jsonl")).unwrap();
    assert!(
        !public_bytes
            .windows(b"inspect".len())
            .any(|window| window == b"inspect")
    );
    assert!(!manifest.root.join("machine-inputs.jsonl").exists());
}

#[test]
fn route_event_is_typed_and_uses_public_refs() {
    let store = RunAssetStore::new_with_fixed_clock(
        RunAssetSink::Root(test_temp_dir("durable-route")),
        1_700_000_000_123,
    );
    let mut manifest = store.start_run_manifest("run-route").unwrap();
    let input = TopologyDecisionInput {
        source: TopologyDecisionSource::Runtime,
        source_native_id: "route:route-0".to_string(),
        flow_lock_id: "flow-a".to_string(),
        route_index: 0,
        route_id: "route-0".to_string(),
        predicate: "exists(artifact.ready)".to_string(),
        for_each: None,
        source_artifact_id: Some("artifact-ready".to_string()),
        trigger_fact_ref: "artifact.ready".to_string(),
        trigger_fact_version: 1,
        planned_activation_ids: vec!["finish".to_string()],
        applied_activation_ids: vec!["finish".to_string()],
        causal_id: None,
        correlation_id: None,
    };
    store
        .record_topology_decision(&mut manifest, input.clone())
        .unwrap();
    store
        .record_topology_decision(&mut manifest, input)
        .unwrap();

    let events = events_of_kind(&manifest, "route.decided");
    assert_eq!(events.len(), 1);
    let payload = &events[0]["data"]["payload"];
    for field in ["flow_ref", "route_ref", "trigger_ref"] {
        assert!(payload[field].as_str().unwrap().starts_with("sha256:"));
    }
    assert_eq!(payload["route_index"], 0);
    assert_eq!(payload["trigger_version"], 1);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(!rendered.contains("run-route"));
    assert!(!rendered.contains("artifact-ready"));
}

#[test]
fn native_session_lifecycle_is_exactly_once_across_two_compaction_cycles() {
    let store = RunAssetStore::new_with_fixed_clock(
        RunAssetSink::Root(test_temp_dir("durable-session")),
        1_700_000_000_123,
    );
    let mut manifest = store.start_run_manifest("run-session").unwrap();
    for _ in 0..2 {
        store
            .record_session_association(
                &mut manifest,
                "native-session",
                SessionRelation::Orchestrates,
                None,
                "codex",
                None,
            )
            .unwrap();
        store
            .record_session_association(
                &mut manifest,
                "native-session",
                SessionRelation::Executes,
                Some("root"),
                "codex",
                None,
            )
            .unwrap();
    }
    for cycle in 0..2 {
        for (hook, suffix) in [
            ("compaction_pending", "start"),
            ("compaction_finished", "finish"),
        ] {
            store
                .record_hook_fact(
                    &mut manifest,
                    HookFactInput {
                        session_id: "native-session".to_string(),
                        activation_id: Some("root".to_string()),
                        hook: hook.to_string(),
                        source_native_id: format!("compaction-{cycle}-{suffix}"),
                        detail: HookFactDetail::Compaction,
                        causal_id: None,
                        correlation_id: Some(format!("compaction-{cycle}")),
                    },
                )
                .unwrap();
        }
    }
    for _ in 0..2 {
        store
            .record_session_association(
                &mut manifest,
                "native-session",
                SessionRelation::Ended,
                Some("root"),
                "codex",
                Some(0),
            )
            .unwrap();
    }

    assert_eq!(events_of_kind(&manifest, "agent_session.started").len(), 1);
    assert_eq!(events_of_kind(&manifest, "agent_session.bound").len(), 1);
    assert_eq!(events_of_kind(&manifest, "agent_session.ended").len(), 1);
    let started = events_of_kind(&manifest, "context_compaction.started")
        .into_iter()
        .map(|event| {
            event["data"]["payload"]["context_generation"]
                .as_u64()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let finished = events_of_kind(&manifest, "context_compaction.finished")
        .into_iter()
        .map(|event| {
            event["data"]["payload"]["context_generation"]
                .as_u64()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(started, vec![0, 1]);
    assert_eq!(finished, vec![1, 2]);
    let rendered = serde_json::to_string(&journal_events(&manifest)).unwrap();
    assert!(!rendered.contains("native-session"));
}

#[test]
fn operator_mcp_hides_hook_only_tools_without_creating_runs() {
    let root = test_temp_dir("durable-hidden-hooks");
    let store = RunAssetStore::new(RunAssetSink::Root(root));
    let missing = store.run_root("run-missing").unwrap();
    let mut server =
        McpServer::with_tmux_runner_and_run_asset_store(RecordingRunner::default(), store);
    let response = call_tool(
        &mut server,
        1,
        "record_hook_fact",
        json!({
            "run_id": "run-missing",
            "session_id": "native-session",
            "hook": "compaction_pending"
        }),
    );
    assert_eq!(response["error"]["message"], "unknown tool");
    assert!(!missing.exists());
}

fn _assert_path_is_relative(path: &Path) {
    assert!(!path.is_absolute());
}
