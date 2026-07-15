#[path = "support/driver_flows.rs"]
#[allow(dead_code)]
mod driver_flows;
#[path = "driver_ipc/support.rs"]
#[allow(dead_code)]
mod driver_support;
#[path = "support/driver_tmux.rs"]
mod driver_tmux_support;

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use humanize_plugin::flow::{
    ContractArtifact, ContractCompletion, FlowCheckMode, FlowContract, FlowDraft, FlowNode,
    FlowPolicies, FlowQosIntent, FlowResource, QosUrgency, ResourceKind, flow_lock,
};
use humanize_plugin::input_ledger::{MachineInputRecord, MachineInputStatus};
use humanize_plugin::run_assets::{
    HookFactDetail, HookFactInput, RunAssetActivationUpdate, RunAssetManifest, RunAssetSink,
    RunAssetStore, RunAssetTmuxTarget, SessionRelation, TopologyDecisionInput,
    TopologyDecisionSource,
};
use humanize_plugin::runtime::{NodeSpec, RunStatus, Runtime};
use serde_json::{Value, json};

use driver_flows::reviewed_lock_package;
use driver_support::DriverFixture;

fn test_temp_dir(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(name);
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    path
}

fn draft() -> FlowDraft {
    FlowDraft {
        nodes: vec![FlowNode {
            id: "root".to_string(),
            ..FlowNode::default()
        }],
        resources: vec![FlowResource {
            id: "README.md".to_string(),
            kind: ResourceKind::Readme,
            source: "Use Humanize to inspect this library.".to_string(),
        }],
        ..FlowDraft::default()
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn journal_events(manifest: &RunAssetManifest) -> Vec<Value> {
    read_jsonl(&manifest.root.join("records/events.jsonl"))
}

fn journal_events_at(root: &Path) -> Vec<Value> {
    read_jsonl(&root.join("records/events.jsonl"))
}

fn assert_public_journal_shape(manifest: &RunAssetManifest, expected_status: &str) {
    let disk = read_json(&manifest.manifest_path);
    assert_eq!(disk["journal"]["schema_name"], "humanize.public_journal");
    assert_eq!(disk["journal"]["schema_major"], 1);
    assert_eq!(disk["journal"]["path"], "records/events.jsonl");
    assert_eq!(disk["journal"]["status"], expected_status);
    assert_eq!(
        disk["journal"]["event_count"],
        journal_events(manifest).len() as u64
    );
    assert_eq!(
        disk["journal"]["last_seq"],
        disk["journal"]["event_count"].as_u64().unwrap()
    );
    assert!(
        disk["journal"]["current_sha256"]
            .as_str()
            .is_some_and(|hash| hash.starts_with("sha256:"))
    );
    if expected_status == "sealed" {
        assert_eq!(
            disk["journal"]["final_sha256"],
            disk["journal"]["current_sha256"]
        );
    } else {
        assert_eq!(disk["journal"]["final_sha256"], Value::Null);
    }
}

fn assert_journal_sequence_and_ids(events: &[Value]) {
    let mut ids = BTreeSet::new();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["schema_name"], "humanize.public_journal.event");
        assert_eq!(event["schema_major"], 1);
        assert_eq!(event["seq"], (index + 1) as u64);
        assert!(
            event["run_ref"]
                .as_str()
                .is_some_and(|value| value.starts_with("sha256:"))
        );
        assert!(event["occurred_at_ms"].as_u64().unwrap() > 0);
        let event_id = event["event_id"].as_str().unwrap();
        assert!(ids.insert(event_id.to_string()), "duplicate {event_id}");
    }
}

fn machine_input_record(run_id: &str) -> MachineInputRecord {
    MachineInputRecord {
        run_id: run_id.to_string(),
        activation_id: "root".to_string(),
        pane_id: "%8".to_string(),
        allocation_generation: 0,
        started_at_ms: 1_700_000_000_100,
        submitted_at_ms: 1_700_000_000_120,
        payload_hash: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_string(),
        normalized_text: "private prompt body".to_string(),
        submit_key_count: 1,
        transaction_id: "machine-input:abc".to_string(),
        status: MachineInputStatus::Submitted,
    }
}

fn record_terminal_runtime_completion(store: &RunAssetStore, manifest: &mut RunAssetManifest) {
    let mut runtime = Runtime::default();
    runtime
        .start_run(&manifest.run_id, vec![NodeSpec::new("root")])
        .unwrap();
    runtime
        .set_run_status(&manifest.run_id, RunStatus::Completed)
        .unwrap();
    for event in runtime.events() {
        store.record_runtime_event(manifest, event).unwrap();
    }
}

#[test]
fn public_journal_is_the_only_records_authority_and_replays_core_facts() {
    let root = test_temp_dir("public-journal-authority");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let lock = flow_lock(&draft(), FlowCheckMode::Core).unwrap();
    let mut manifest = store.start_run_manifest("run-public-journal").unwrap();

    assert!(!manifest.root.join("records/storage.jsonl").exists());
    assert!(!manifest.root.join("records/index.json").exists());

    store
        .persist_flow_revision(&mut manifest, &lock, "hash:abc123", "approved")
        .unwrap();
    store
        .record_session_association(
            &mut manifest,
            "host-master",
            SessionRelation::Orchestrates,
            None,
            "codex",
            None,
        )
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
                    window_name: "flow-a".to_string(),
                    pane_id: "%8".to_string(),
                    allocation_generation: 0,
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
        .record_hook_fact(
            &mut manifest,
            HookFactInput {
                session_id: "host-a".to_string(),
                activation_id: Some("root".to_string()),
                hook: "compaction_pending".to_string(),
                source_native_id: "hook-compact-start".to_string(),
                detail: HookFactDetail::Compaction,
                causal_id: None,
                correlation_id: Some("compact-1".to_string()),
            },
        )
        .unwrap();
    store
        .record_hook_fact(
            &mut manifest,
            HookFactInput {
                session_id: "host-a".to_string(),
                activation_id: Some("root".to_string()),
                hook: "compaction_finished".to_string(),
                source_native_id: "hook-compact-finish".to_string(),
                detail: HookFactDetail::Compaction,
                causal_id: None,
                correlation_id: Some("compact-1".to_string()),
            },
        )
        .unwrap();
    store
        .record_qos_intent(
            &mut manifest,
            &FlowQosIntent {
                urgency: QosUrgency::Interactive,
                completion_target: Some("artifact.summary".to_string()),
            },
        )
        .unwrap();
    store
        .record_topology_decision(
            &mut manifest,
            TopologyDecisionInput {
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
                causal_id: Some("artifact-ready".to_string()),
                correlation_id: None,
            },
        )
        .unwrap();
    store
        .record_machine_input(
            &mut manifest,
            "node_prompt",
            &machine_input_record("run-public-journal"),
        )
        .unwrap();

    let mut runtime = Runtime::default();
    runtime
        .start_run("run-public-journal", vec![NodeSpec::new("root")])
        .unwrap();
    runtime
        .deliver_artifact("run-public-journal", "root", "summary", "ready")
        .unwrap();
    for event in runtime.events() {
        store.record_runtime_event(&mut manifest, event).unwrap();
    }

    let events = journal_events(&manifest);
    assert_journal_sequence_and_ids(&events);
    let kinds = events
        .iter()
        .map(|event| event["kind"].as_str().unwrap().to_string())
        .collect::<BTreeSet<_>>();
    for expected in [
        "run.started",
        "flow_revision.prepared",
        "flow_revision.applied",
        "activation.created",
        "activation.status",
        "agent_session.started",
        "hook.observed",
        "context_compaction.started",
        "context_compaction.finished",
        "qos.observed",
        "route.decided",
        "machine_input.delivered",
        "artifact.recorded",
    ] {
        assert!(kinds.contains(expected), "missing {expected}: {kinds:?}");
    }
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(!serialized.contains("private prompt body"));
    assert!(!serialized.contains("normalized_text"));
    assert!(!serialized.contains("readiness_nonce"));
    assert!(!serialized.contains(manifest.root.to_string_lossy().as_ref()));
    assert_public_journal_shape(&manifest, "open");
}

#[test]
fn terminal_public_journal_seals_and_torn_tail_is_quarantined() {
    let root = test_temp_dir("public-journal-seal");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let lock = flow_lock(&draft(), FlowCheckMode::Core).unwrap();
    let mut manifest = store.start_run_manifest("run-public-journal").unwrap();
    store
        .persist_flow_revision(&mut manifest, &lock, "hash:abc123", "approved")
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
                    window_name: "flow-a".to_string(),
                    pane_id: "%8".to_string(),
                    allocation_generation: 0,
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
        .mark_activation_resource_cleanup(&mut manifest, "root", "complete", None)
        .unwrap();
    record_terminal_runtime_completion(&store, &mut manifest);

    assert!(manifest.completion.complete);
    assert!(
        journal_events(&manifest)
            .iter()
            .any(|event| event["kind"] == "activation.completed")
    );
    assert_public_journal_shape(&manifest, "sealed");
    let sealed = read_json(&manifest.manifest_path);
    let sealed_hash = sealed["journal"]["final_sha256"].clone();

    let events_path = manifest.root.join("records/events.jsonl");
    fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .unwrap()
        .write_all(br#"{"seq":999"#)
        .unwrap();
    let rebuilt = store.rebuild_record_index(&manifest).unwrap();
    assert_eq!(
        rebuilt["files"]["events"]["relative_path"],
        "records/events.jsonl"
    );
    let recovered = read_json(&manifest.manifest_path);
    assert_eq!(recovered["journal"]["status"], "corrupt");
    assert_eq!(recovered["journal"]["final_sha256"], sealed_hash);
    assert!(manifest.root.join("records/quarantine").exists());
}

#[test]
fn sealed_public_journal_rejects_later_appends() {
    let root = test_temp_dir("public-journal-post-seal");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let mut manifest = store.start_run_manifest("run-public-journal").unwrap();
    store
        .start_activation_capture(
            &mut manifest,
            RunAssetActivationUpdate {
                activation_id: "root".to_string(),
                node_id: "root".to_string(),
                tmux: RunAssetTmuxTarget {
                    session_id: "host-a".to_string(),
                    window_id: "%7".to_string(),
                    window_name: "flow-a".to_string(),
                    pane_id: "%8".to_string(),
                    allocation_generation: 0,
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
        .mark_activation_resource_cleanup(&mut manifest, "root", "complete", None)
        .unwrap();
    record_terminal_runtime_completion(&store, &mut manifest);
    assert_public_journal_shape(&manifest, "sealed");

    let err = store
        .record_topology_decision(
            &mut manifest,
            TopologyDecisionInput {
                source: TopologyDecisionSource::Runtime,
                source_native_id: "route:after-seal".to_string(),
                flow_lock_id: "flow-a".to_string(),
                route_index: 0,
                route_id: "after-seal".to_string(),
                predicate: "exists(artifact.ready)".to_string(),
                for_each: None,
                source_artifact_id: None,
                trigger_fact_ref: "artifact.ready".to_string(),
                trigger_fact_version: 1,
                planned_activation_ids: vec!["finish".to_string()],
                applied_activation_ids: vec!["finish".to_string()],
                causal_id: None,
                correlation_id: None,
            },
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("sealed"),
        "unexpected append error: {err}"
    );
}

#[test]
fn conflicting_public_journal_retry_is_an_error() {
    let root = test_temp_dir("public-journal-conflicting-retry");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_123);
    let mut manifest = store.start_run_manifest("run-public-journal").unwrap();
    store
        .record_topology_decision(
            &mut manifest,
            TopologyDecisionInput {
                source: TopologyDecisionSource::Runtime,
                source_native_id: "route:route-0".to_string(),
                flow_lock_id: "flow-a".to_string(),
                route_index: 0,
                route_id: "route-0".to_string(),
                predicate: "exists(artifact.ready)".to_string(),
                for_each: None,
                source_artifact_id: None,
                trigger_fact_ref: "artifact.ready".to_string(),
                trigger_fact_version: 1,
                planned_activation_ids: vec!["follow".to_string()],
                applied_activation_ids: vec!["follow".to_string()],
                causal_id: None,
                correlation_id: None,
            },
        )
        .unwrap();
    let err = store
        .record_topology_decision(
            &mut manifest,
            TopologyDecisionInput {
                source: TopologyDecisionSource::Runtime,
                source_native_id: "route:route-0".to_string(),
                flow_lock_id: "flow-a".to_string(),
                route_index: 0,
                route_id: "route-0".to_string(),
                predicate: "exists(artifact.ready)".to_string(),
                for_each: None,
                source_artifact_id: None,
                trigger_fact_ref: "artifact.ready".to_string(),
                trigger_fact_version: 1,
                planned_activation_ids: vec!["other".to_string()],
                applied_activation_ids: vec!["other".to_string()],
                causal_id: None,
                correlation_id: None,
            },
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("conflicts"),
        "unexpected retry error: {err}"
    );
}

#[test]
fn production_driver_commits_runtime_facts_to_public_journal() {
    let fixture = DriverFixture::new("public-journal-driver-runtime");
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let mut driver =
        fixture.spawn_with_env("run-driver-journal", &[("HUMANIZE_TMUX_BIN", &fake_tmux)]);
    let bound = fixture.request(json!({
        "id":"bind",
        "token":fixture.token,
        "op":"bind_run",
        "run_id":"run-driver-journal",
        "flow_lock":reviewed_lock_package(),
        "tmux":{"enabled":true,"session":"host-a","window":"flow-a","agent_command":"humanize-test-agent"}
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    let root_activation = bound["activation_ids"][0].as_str().unwrap();
    let delivered = fixture.request(json!({
        "id":"deliver",
        "token":fixture.token,
        "op":"deliver_artifact",
        "run_id":"run-driver-journal",
        "activation_id": root_activation,
        "artifact_id": "brief",
        "payload": "public artifact content"
    }));
    assert_eq!(delivered["ok"], true, "{delivered}");

    let run_root = fixture.run_root("run-driver-journal");
    let events = journal_events_at(&run_root);
    let kinds = events
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    for expected in [
        "run.started",
        "run.status",
        "flow_revision.applied",
        "activation.created",
        "activation.status",
        "artifact.recorded",
        "route.decided",
        "agent_session.started",
        "agent_session.bound",
    ] {
        assert!(kinds.contains(expected), "missing {expected}: {kinds:?}");
    }
    let artifact_event = events
        .iter()
        .find(|event| event["kind"] == "artifact.recorded")
        .expect("artifact delivery should be journaled by the driver");
    let artifact_ref = artifact_event["data"]["payload"]["content"]["path"]
        .as_str()
        .expect("artifact delivery should reference canonical content");
    assert_eq!(
        fs::read_to_string(run_root.join(artifact_ref)).unwrap(),
        "public artifact content"
    );
    let public_events = fs::read_to_string(run_root.join("records/events.jsonl")).unwrap();
    assert!(!public_events.contains("public artifact content"));
    let public_manifest = fs::read_to_string(run_root.join("manifest.json")).unwrap();
    assert!(!public_manifest.contains("public artifact content"));
    let public_tree = collect_public_tree_bytes(&run_root);
    for forbidden in [
        "fake-native-session",
        "host-a:%7",
        "humanize-test-agent",
        "readiness_nonce",
        "participant_credential",
        "driver/",
    ] {
        assert!(
            !contains_bytes(&public_tree, forbidden.as_bytes()),
            "leaked {forbidden}"
        );
    }
    driver.shutdown();
}

#[test]
fn continuous_driver_shutdown_keeps_public_journal_open() {
    let fixture = DriverFixture::new("public-journal-continuous-open");
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let mut driver =
        fixture.spawn_with_env("run-continuous-open", &[("HUMANIZE_TMUX_BIN", &fake_tmux)]);
    let bound = fixture.request(json!({
        "id":"bind",
        "token":fixture.token,
        "op":"bind_run",
        "run_id":"run-continuous-open",
        "flow_lock":reviewed_lock_package(),
        "run_mode":"continuous",
        "tmux":{"enabled":true,"session":"host-a","window":"flow-a","agent_command":"humanize-test-agent"}
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    let root_activation = bound["activation_ids"][0].as_str().unwrap();
    let delivered = fixture.request(json!({
        "id":"deliver",
        "token":fixture.token,
        "op":"deliver_artifact",
        "run_id":"run-continuous-open",
        "activation_id": root_activation,
        "artifact_id": "brief",
        "payload": "content"
    }));
    assert_eq!(delivered["ok"], true, "{delivered}");
    driver.shutdown();

    let manifest = read_json(
        &fixture
            .run_root("run-continuous-open")
            .join("manifest.json"),
    );
    assert_eq!(manifest["journal"]["status"], "open");
}

#[test]
fn forged_active_seal_is_quarantined_and_terminal_seal_prevents_side_effects() {
    let active = DriverFixture::new("public-journal-forged-active-seal");
    let mut active_driver = active.spawn("run-forged-active-seal");
    let bound = active.request(json!({
        "id": "bind-active",
        "token": active.token,
        "op": "bind_run",
        "run_id": "run-forged-active-seal",
        "flow_lock": single_node_lock_package()
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    let active_root = active.run_root("run-forged-active-seal");
    write_current_journal_seal(&active_root);

    let delivered = active.request(json!({
        "id": "deliver-through-forged-seal",
        "token": active.token,
        "op": "deliver_artifact",
        "run_id": "run-forged-active-seal",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": "active publication"
    }));
    assert_eq!(delivered["ok"], true, "{delivered}");
    assert!(!active_root.join("records/journal-seal.json").exists());
    assert!(
        active_root
            .join("records/quarantine/invalid-seals")
            .exists()
    );
    active_driver.shutdown();

    let terminal = DriverFixture::new("public-journal-terminal-side-effects");
    let mut terminal_driver = terminal.spawn("run-terminal-side-effects");
    let bound = terminal.request(json!({
        "id": "bind-manual",
        "token": terminal.token,
        "op": "bind_run",
        "run_id": "run-terminal-side-effects",
        "run_mode": "manual",
        "flow_lock": single_node_lock_package()
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    let delivered = terminal.request(json!({
        "id": "deliver-terminal-brief",
        "token": terminal.token,
        "op": "deliver_artifact",
        "run_id": "run-terminal-side-effects",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": "terminal publication"
    }));
    assert_eq!(delivered["ok"], true, "{delivered}");
    let stopped = terminal.request(json!({
        "id": "observe-terminal-stop",
        "token": terminal.token,
        "op": "observe_stop",
        "run_id": "run-terminal-side-effects",
        "activation_id": "root",
        "reason": "done"
    }));
    assert_eq!(stopped["run_status"], "quiescent", "{stopped}");
    let completed = terminal.request(json!({
        "id": "complete-manual-run",
        "token": terminal.token,
        "op": "complete",
        "run_id": "run-terminal-side-effects"
    }));
    assert_eq!(completed["run_status"], "completed", "{completed}");

    let terminal_root = terminal.run_root("run-terminal-side-effects");
    let manifest = read_json(&terminal_root.join("manifest.json"));
    assert_eq!(manifest["journal"]["status"], "sealed");
    let public_before = collect_public_tree_bytes(&terminal_root);
    let private_events_before =
        fs::read(terminal.run_events_path("run-terminal-side-effects")).unwrap();
    let files_before = public_file_paths(&terminal_root);
    let rejected = terminal.request(json!({
        "id": "deliver-after-terminal-seal",
        "token": terminal.token,
        "op": "deliver_artifact",
        "run_id": "run-terminal-side-effects",
        "activation_id": "root",
        "artifact_id": "after-seal",
        "payload": "post-seal-secret"
    }));
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(collect_public_tree_bytes(&terminal_root), public_before);
    assert_eq!(
        fs::read(terminal.run_events_path("run-terminal-side-effects")).unwrap(),
        private_events_before
    );
    assert_eq!(public_file_paths(&terminal_root), files_before);
    assert!(!contains_bytes(
        &collect_public_tree_bytes(&terminal_root),
        b"post-seal-secret"
    ));
    terminal_driver.shutdown();
}

#[test]
fn driver_run_keeps_public_tree_free_of_private_driver_material() {
    let fixture = DriverFixture::new("public-private-driver-split");
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let mut driver =
        fixture.spawn_with_env("run-private-split", &[("HUMANIZE_TMUX_BIN", &fake_tmux)]);
    let bound = fixture.request(json!({
        "id":"bind",
        "token":fixture.token,
        "op":"bind_run",
        "run_id":"run-private-split",
        "flow_lock":reviewed_lock_package(),
        "tmux":{"enabled":true,"session":"host-a","window":"flow-a","agent_command":"humanize-test-agent"}
    }));
    assert_eq!(bound["ok"], true, "{bound}");

    let run_root = fixture.run_root("run-private-split");
    assert!(run_root.join("records/events.jsonl").exists());
    assert!(!run_root.join("driver").exists());
    assert!(!run_root.join("machine-inputs.jsonl").exists());
    let public_tree = collect_public_tree_bytes(&run_root);
    for forbidden in [
        "readiness_nonce",
        "participant_credential",
        "ipc-token",
        "native-codex-session",
        "driver-events",
        "snapshot.json",
    ] {
        assert!(
            !contains_bytes(&public_tree, forbidden.as_bytes()),
            "leaked {forbidden}"
        );
    }

    let runtime_root = fixture.runtime_root.clone();
    assert!(runtime_root.exists());
    assert!(contains_bytes(
        &collect_public_tree_bytes(&runtime_root),
        b"driver-events"
    ));
    driver.shutdown();

    let mut restarted =
        fixture.spawn_with_env("run-private-split", &[("HUMANIZE_TMUX_BIN", &fake_tmux)]);
    let status = fixture.request(json!({
        "id":"status",
        "token":fixture.token,
        "op":"status",
        "run_id":"run-private-split"
    }));
    assert_eq!(status["ok"], true, "{status}");
    restarted.shutdown();
}

fn collect_public_tree_bytes(root: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    collect_public_tree_bytes_inner(root, root, &mut out);
    out
}

fn collect_public_tree_bytes_inner(root: &Path, path: &Path, out: &mut Vec<u8>) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    let relative = path.strip_prefix(root).unwrap_or(path);
    out.extend_from_slice(relative.as_os_str().as_encoded_bytes());
    out.push(b'\n');
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            collect_public_tree_bytes_inner(root, &entry, out);
        }
    } else if metadata.is_file()
        && let Ok(bytes) = fs::read(path)
    {
        out.extend_from_slice(&bytes);
        out.push(b'\n');
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn write_current_journal_seal(run_root: &Path) {
    let manifest = read_json(&run_root.join("manifest.json"));
    let journal = &manifest["journal"];
    fs::write(
        run_root.join("records/journal-seal.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_name": "humanize.public_journal",
            "schema_major": 1,
            "final_sha256": journal["current_sha256"],
            "event_count": journal["event_count"],
            "last_seq": journal["last_seq"]
        }))
        .unwrap(),
    )
    .unwrap();
}

fn single_node_lock_package() -> Value {
    let draft = FlowDraft {
        nodes: vec![FlowNode {
            id: "root".to_string(),
            contract_id: Some("contract.root".to_string()),
            ..FlowNode::default()
        }],
        contracts: vec![FlowContract {
            id: "contract.root".to_string(),
            completion: Some(ContractCompletion::AllArtifacts),
            artifacts: vec![ContractArtifact {
                id: "brief".to_string(),
                schema_resource_id: Some("schema.brief".to_string()),
            }],
        }],
        resources: vec![
            FlowResource {
                id: "README.md".to_string(),
                kind: ResourceKind::Readme,
                source: "Single-node terminal fixture.".to_string(),
            },
            FlowResource {
                id: "schema.brief".to_string(),
                kind: ResourceKind::Schema,
                source: "brief".to_string(),
            },
        ],
        policies: FlowPolicies::default(),
        ..FlowDraft::default()
    };
    serde_json::to_value(flow_lock(&draft, FlowCheckMode::Core).unwrap()).unwrap()
}

fn public_file_paths(root: &Path) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    collect_public_file_paths(root, root, &mut paths);
    paths
}

fn collect_public_file_paths(root: &Path, path: &Path, paths: &mut BTreeSet<String>) {
    let metadata = fs::symlink_metadata(path).unwrap();
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            collect_public_file_paths(root, &entry, paths);
        }
    } else {
        assert!(metadata.is_file(), "public tree contains a special file");
        paths.insert(
            path.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/"),
        );
    }
}
