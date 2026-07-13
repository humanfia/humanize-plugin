use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use humanize_plugin::flow::{
    ContractArtifact, ContractCompletion, FlowCheckMode, FlowContract, FlowDraft, FlowExportFormat,
    FlowNode, FlowResource, ResourceKind, flow_export, flow_lock,
};
use humanize_plugin::run_assets::{
    RunAssetActivationUpdate, RunAssetManifest, RunAssetSink, RunAssetStore, RunAssetTmuxTarget,
};
use serde_json::Value;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

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
            id: "readme.main".to_string(),
            kind: ResourceKind::Readme,
            source: "inline:Use Humanize to audit this library.".to_string(),
        }],
        ..FlowDraft::default()
    }
}

fn contract_draft_without_effects() -> FlowDraft {
    FlowDraft {
        nodes: vec![FlowNode {
            id: "root".to_string(),
            contract_id: Some("contract.root".to_string()),
            ..FlowNode::default()
        }],
        contracts: vec![FlowContract {
            id: "contract.root".to_string(),
            completion: Some(ContractCompletion::AllArtifacts),
            artifacts: vec![ContractArtifact {
                id: "report".to_string(),
                schema_resource_id: Some("schema.report".to_string()),
            }],
        }],
        resources: vec![
            FlowResource {
                id: "readme.main".to_string(),
                kind: ResourceKind::Readme,
                source: "inline:Use Humanize to audit this library.".to_string(),
            },
            FlowResource {
                id: "schema.report".to_string(),
                kind: ResourceKind::Schema,
                source: "inline:report".to_string(),
            },
        ],
        ..FlowDraft::default()
    }
}

#[test]
fn explicit_humanize_runs_dir_uses_deterministic_run_path() {
    let runs_dir = test_temp_dir("run-assets-humanize-runs-dir");
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(runs_dir.clone()));

    let root = store.run_root("run-a").unwrap();

    let relative = root.strip_prefix(runs_dir).unwrap().to_string_lossy();
    assert!(relative.starts_with("run-sha256-"));
    assert!(relative.contains("-run-a"));
}

#[test]
fn fallback_sink_stays_under_cache_and_never_uses_project_local_humanize() {
    let home = test_temp_dir("run-assets-cache-home");
    let store = RunAssetStore::new(RunAssetSink::CacheHome(home.clone()));

    let root = store.run_root("run-a").unwrap();

    assert!(root.starts_with(home.join(".cache").join("humanize").join("runs")));
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("run-sha256-")
    );
    assert!(!root.to_string_lossy().contains("/.humanize/"));
}

#[test]
fn start_run_manifest_is_persisted_incomplete_before_flow_lock() {
    let root = test_temp_dir("run-assets-start-manifest");
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));

    let manifest = store.start_run_manifest("run-start").unwrap();

    assert!(manifest.storage.run_directory.starts_with("run-sha256-"));
    assert_ne!(manifest.storage.run_directory, "run-start");
    let manifest_path = root
        .join(&manifest.storage.run_directory)
        .join("manifest.json");
    assert_eq!(manifest.manifest_path, manifest_path);
    assert!(manifest_path.exists());
    assert!(!manifest.root.join("flow/current/flow-lock.json").exists());

    let manifest_json: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest_json["run_id"], "run-start");
    assert_eq!(manifest_json["storage"]["raw_run_id"], "run-start");
    assert_eq!(
        manifest_json["storage"]["run_directory"],
        manifest.storage.run_directory
    );
    assert_eq!(
        manifest_json["storage"]["run_relative_path"],
        manifest.storage.run_directory
    );
    assert_eq!(manifest_json["flow"]["status"], "pending");
    assert_eq!(manifest_json["flow"]["complete"], false);
    assert_eq!(manifest_json["flow"]["current_export_path"], Value::Null);
    assert_eq!(manifest_json["flow"]["revisions"], Value::Array(Vec::new()));
    assert_eq!(
        manifest_json["artifact_paths"]["manifest_relative_path"],
        "manifest.json"
    );
    assert_eq!(manifest_json["artifact_paths"]["flow_current"], Value::Null);
    assert!(!manifest_path.to_string_lossy().contains("/.humanize/"));
}

#[test]
fn raw_ids_keep_identity_and_hash_prefixed_discoverable_storage_path() {
    let root = test_temp_dir("run-assets-sanitized-run-id");
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));

    let manifest = store.start_run_manifest("worker:artifact/0").unwrap();

    let manifest_json: Value =
        serde_json::from_str(&fs::read_to_string(&manifest.manifest_path).unwrap()).unwrap();
    assert_eq!(manifest_json["run_id"], "worker:artifact/0");
    assert_eq!(manifest_json["storage"]["raw_run_id"], "worker:artifact/0");
    assert_ne!(
        manifest_json["storage"]["run_directory"],
        "worker:artifact/0"
    );
    assert!(
        manifest_json["storage"]["run_directory"]
            .as_str()
            .unwrap()
            .starts_with("run-sha256-")
    );
    assert_eq!(
        manifest_json["storage"]["run_relative_path"],
        manifest_json["storage"]["run_directory"]
    );
    assert_eq!(
        root.join(
            manifest_json["storage"]["run_relative_path"]
                .as_str()
                .unwrap()
        ),
        manifest.root
    );
}

#[test]
fn storage_segments_are_injective_for_safe_and_normalized_unsafe_ids() {
    let root = test_temp_dir("run-assets-injective-run-id");
    let store = RunAssetStore::new(RunAssetSink::Root(root));

    let unsafe_manifest = store.start_run_manifest("worker:artifact/0").unwrap();
    let safe_manifest = store.start_run_manifest("worker_artifact_0").unwrap();

    assert_ne!(
        unsafe_manifest.storage.run_directory,
        safe_manifest.storage.run_directory
    );
    assert!(
        unsafe_manifest
            .storage
            .run_directory
            .starts_with("run-sha256-")
    );
    assert!(
        safe_manifest
            .storage
            .run_directory
            .starts_with("run-sha256-")
    );
    assert_ne!(safe_manifest.storage.run_directory, "worker_artifact_0");
}

#[test]
fn storage_segments_bound_long_run_and_activation_ids_without_losing_hash_identity() {
    let root = test_temp_dir("run-assets-bounded-storage-segments");
    let store = RunAssetStore::new(RunAssetSink::Root(root));
    let run_id_a = format!("run-{}-a", "x".repeat(1_000));
    let run_id_b = format!("run-{}-b", "x".repeat(1_000));

    let mut manifest_a = store.start_run_manifest(&run_id_a).unwrap();
    let manifest_b = store.start_run_manifest(&run_id_b).unwrap();

    assert_eq!(manifest_a.storage.run_directory.len(), 180);
    assert_eq!(manifest_b.storage.run_directory.len(), 180);
    assert_ne!(
        manifest_a.storage.run_directory,
        manifest_b.storage.run_directory
    );
    assert!(manifest_a.storage.run_directory.is_ascii());

    let activation_id_a = format!("worker:{}:a", "artifact/".repeat(200));
    let activation_id_b = format!("worker:{}:b", "artifact/".repeat(200));
    store
        .register_expected_activation(&mut manifest_a, &activation_id_a, "worker", "tmux")
        .unwrap();
    store
        .register_expected_activation(&mut manifest_a, &activation_id_b, "worker", "tmux")
        .unwrap();

    let segment_a = manifest_a.activations[&activation_id_a]
        .metadata_path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let segment_b = manifest_a.activations[&activation_id_b]
        .metadata_path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(segment_a.len(), 180);
    assert_eq!(segment_b.len(), 180);
    assert_ne!(segment_a, segment_b);
    assert!(segment_a.is_ascii());

    let unicode_marker = char::from_u32(0x754c).unwrap().to_string();
    let unicode_run_id = unicode_marker.repeat(1_000);
    let unicode_manifest = store.start_run_manifest(&unicode_run_id).unwrap();
    assert_eq!(unicode_manifest.storage.run_directory.len(), 180);
    assert!(unicode_manifest.storage.run_directory.is_ascii());
}

#[test]
fn start_run_manifest_create_new_rejects_existing_storage() {
    let root = test_temp_dir("run-assets-create-new");
    let store = RunAssetStore::new(RunAssetSink::Root(root));

    let first = store.start_run_manifest("run-reuse").unwrap();
    let second = store.start_run_manifest("run-reuse");

    assert!(second.is_err());
    assert!(first.manifest_path.exists());
}

#[test]
fn start_run_manifest_rejects_existing_storage_with_different_raw_id() {
    let root = test_temp_dir("run-assets-storage-collision");
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));
    let run_root = store.run_root("worker:artifact/0").unwrap();
    fs::create_dir_all(&run_root).unwrap();
    fs::write(
        run_root.join("manifest.json"),
        serde_json::json!({
            "run_id": "literal-other",
            "storage": {
                "raw_run_id": "literal-other"
            }
        })
        .to_string(),
    )
    .unwrap();

    let err = store
        .start_run_manifest("worker:artifact/0")
        .expect_err("colliding storage must be rejected");

    assert!(err.to_string().contains("storage hash collision"));
}

#[test]
fn no_effect_flow_preserves_historical_canonical_bytes_and_lock_id() {
    let lock = flow_lock(&contract_draft_without_effects(), FlowCheckMode::Core).unwrap();
    let exported = flow_export(&lock, FlowExportFormat::Json);
    let exported_json: Value = serde_json::from_str(&exported).unwrap();
    let content = exported_json["content"].as_str().unwrap();

    assert!(!content.contains("\"effects\":[]"));
    assert!(content.contains("\"effect_requirements\":[]"));
    assert_eq!(lock.id(), "flk_9c79529ac3fd3e4b");
    assert_eq!(
        content,
        "{\"mode\":\"core\",\"draft\":{\"nodes\":[{\"id\":\"root\",\"contract_id\":\"contract.root\",\"action\":null,\"write_scopes\":[],\"extensions\":[]}],\"contracts\":[{\"id\":\"contract.root\",\"completion\":\"all_artifacts\",\"artifacts\":[{\"id\":\"report\",\"schema_resource_id\":\"schema.report\"}]}],\"routes\":[],\"resources\":[{\"id\":\"readme.main\",\"kind\":\"readme\",\"source\":\"inline:Use Humanize to audit this library.\"},{\"id\":\"schema.report\",\"kind\":\"schema\",\"source\":\"inline:report\"}],\"imports\":[],\"policies\":{\"write_scopes\":[]},\"extensions\":[]},\"adapter_capabilities\":[],\"node_contracts\":[{\"node_id\":\"root\",\"contract_id\":\"contract.root\",\"requires\":[],\"prefers\":[],\"accepts\":[],\"completion_policy\":\"all_artifacts\",\"artifact_requirements\":[{\"id\":\"report\",\"schema_resource_id\":\"schema.report\",\"required\":true}],\"effect_requirements\":[],\"stop_gate\":\"required\"}],\"diagnostics\":[]}"
    );
    assert_eq!(
        exported,
        "{\n  \"id\": \"flk_9c79529ac3fd3e4b\",\n  \"check_mode\": \"core\",\n  \"diagnostics\": [],\n  \"content\": \"{\\\"mode\\\":\\\"core\\\",\\\"draft\\\":{\\\"nodes\\\":[{\\\"id\\\":\\\"root\\\",\\\"contract_id\\\":\\\"contract.root\\\",\\\"action\\\":null,\\\"write_scopes\\\":[],\\\"extensions\\\":[]}],\\\"contracts\\\":[{\\\"id\\\":\\\"contract.root\\\",\\\"completion\\\":\\\"all_artifacts\\\",\\\"artifacts\\\":[{\\\"id\\\":\\\"report\\\",\\\"schema_resource_id\\\":\\\"schema.report\\\"}]}],\\\"routes\\\":[],\\\"resources\\\":[{\\\"id\\\":\\\"readme.main\\\",\\\"kind\\\":\\\"readme\\\",\\\"source\\\":\\\"inline:Use Humanize to audit this library.\\\"},{\\\"id\\\":\\\"schema.report\\\",\\\"kind\\\":\\\"schema\\\",\\\"source\\\":\\\"inline:report\\\"}],\\\"imports\\\":[],\\\"policies\\\":{\\\"write_scopes\\\":[]},\\\"extensions\\\":[]},\\\"adapter_capabilities\\\":[],\\\"node_contracts\\\":[{\\\"node_id\\\":\\\"root\\\",\\\"contract_id\\\":\\\"contract.root\\\",\\\"requires\\\":[],\\\"prefers\\\":[],\\\"accepts\\\":[],\\\"completion_policy\\\":\\\"all_artifacts\\\",\\\"artifact_requirements\\\":[{\\\"id\\\":\\\"report\\\",\\\"schema_resource_id\\\":\\\"schema.report\\\",\\\"required\\\":true}],\\\"effect_requirements\\\":[],\\\"stop_gate\\\":\\\"required\\\"}],\\\"diagnostics\\\":[]}\"\n}"
    );
}

#[test]
fn run_manifest_fixtures_match_rust_struct_schema() {
    assert_fixture_matches(
        "tests/fixtures/run_assets/running_manifest.json",
        fixture_running_manifest(),
    );
    assert_fixture_matches(
        "tests/fixtures/run_assets/completed_manifest.json",
        fixture_completed_manifest(),
    );
}

#[test]
fn pre_records_manifest_fixture_deserializes_without_record_index_field() {
    let manifest_json = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/run_assets/pre_records_manifest.json"),
    )
    .unwrap();

    let manifest: RunAssetManifest = serde_json::from_str(&manifest_json).unwrap();

    assert_eq!(manifest.run_id, "legacy-run");
    assert_eq!(manifest.storage.raw_run_id, "legacy-run");
}

#[test]
fn fixture_storage_hashes_match_real_store_mapping() {
    let running = fixture_running_manifest();
    let root = test_temp_dir("run-assets-fixture-storage-hashes");
    let store = RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_000);
    let worker_manifest = store.start_run_manifest("worker:artifact/0").unwrap();
    let root_manifest = store.start_run_manifest("root").unwrap();

    assert_eq!(
        running.storage.run_directory,
        worker_manifest.storage.run_directory
    );
    assert!(running.storage.run_directory.starts_with("run-sha256-"));
    assert!(
        running.activations["root"]
            .relative_paths
            .metadata
            .starts_with("activations/act-sha256-")
    );
    assert!(
        root_manifest
            .storage
            .run_directory
            .starts_with("run-sha256-")
    );
    assert_ne!(
        worker_manifest.storage.run_directory,
        root_manifest.storage.run_directory
    );
}

#[test]
fn flow_revision_is_prepared_before_runtime_apply_and_committed_after_apply() {
    let root = test_temp_dir("run-assets-flow-commit-boundary");
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));
    let lock = flow_lock(&draft(), FlowCheckMode::Core).unwrap();
    let mut manifest = store.start_run_manifest("run-flow-boundary").unwrap();

    let prepared = store
        .prepare_flow_revision(&mut manifest, &lock, "hash:abc123", "not_required")
        .unwrap();

    let revision_flow_path = manifest.root.join("flow/revisions/rev-0001/flow-lock.json");
    assert_eq!(prepared.revision_id, "rev-0001");
    assert_eq!(
        prepared.relative_path,
        "flow/revisions/rev-0001/flow-lock.json"
    );
    assert_eq!(prepared.apply_state, "prepared");
    assert_eq!(manifest.flow.status, "prepared");
    assert!(!manifest.flow.complete);
    assert_eq!(manifest.flow.current_revision_id, None);
    assert_eq!(manifest.flow.current_export_relative_path, None);
    assert_eq!(manifest.artifact_paths.flow_current_relative_path, None);
    assert_eq!(manifest.flow.revisions[0].apply_state, "prepared");
    assert_eq!(
        fs::read_to_string(&revision_flow_path).unwrap(),
        flow_export(&lock, FlowExportFormat::Json)
    );
    let prepared_json: Value =
        serde_json::from_str(&fs::read_to_string(&manifest.manifest_path).unwrap()).unwrap();
    assert_eq!(prepared_json["flow"]["status"], "prepared");
    assert_eq!(prepared_json["flow"]["complete"], false);
    assert_eq!(
        prepared_json["flow"]["current_export_relative_path"],
        Value::Null
    );
    assert_eq!(
        prepared_json["flow"]["revisions"][0]["apply_state"],
        "prepared"
    );

    store
        .commit_flow_revision_applied(&mut manifest, "rev-0001")
        .unwrap();

    let committed_json: Value =
        serde_json::from_str(&fs::read_to_string(&manifest.manifest_path).unwrap()).unwrap();
    assert_eq!(manifest.flow.status, "complete");
    assert!(manifest.flow.complete);
    assert_eq!(
        manifest.flow.current_revision_id.as_deref(),
        Some("rev-0001")
    );
    assert_eq!(
        manifest.flow.current_export_relative_path.as_deref(),
        Some("flow/revisions/rev-0001/flow-lock.json")
    );
    assert_eq!(manifest.flow.revisions[0].apply_state, "applied");
    assert_eq!(committed_json["flow"]["status"], "complete");
    assert_eq!(
        committed_json["flow"]["current_export_relative_path"],
        "flow/revisions/rev-0001/flow-lock.json"
    );
    assert_eq!(
        committed_json["flow"]["revisions"][0]["apply_state"],
        "applied"
    );
}

#[test]
fn flow_revisions_and_current_export_are_persisted_with_protocol_identity_and_paths() {
    let root = test_temp_dir("run-assets-package");
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));
    let lock = flow_lock(&draft(), FlowCheckMode::Core).unwrap();
    let content_hash = "hash:abc123";
    let mut manifest = store.start_run_manifest("run-package").unwrap();

    let revision = store
        .persist_flow_revision(&mut manifest, &lock, content_hash, "not_required")
        .unwrap();

    let run_root = manifest.root.clone();
    let manifest_path = run_root.join("manifest.json");
    let revision_flow_path = run_root.join("flow/revisions/rev-0001/flow-lock.json");
    assert_eq!(manifest.manifest_path, manifest_path);
    assert_eq!(revision.export_path, revision_flow_path);
    assert_eq!(
        fs::read_to_string(&revision_flow_path).unwrap(),
        flow_export(&lock, FlowExportFormat::Json)
    );

    let manifest_json: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest_json["run_id"], "run-package");
    assert_eq!(
        manifest_json["protocol"]["mcp_protocol_version"],
        "2024-11-05"
    );
    assert_eq!(manifest_json["protocol"]["package_name"], "humanize-plugin");
    assert_eq!(manifest_json["flow"]["main_flow"], true);
    assert_eq!(manifest_json["flow"]["status"], "complete");
    assert_eq!(manifest_json["flow"]["complete"], true);
    assert_eq!(manifest_json["flow"]["current_revision_id"], "rev-0001");
    assert_eq!(
        manifest_json["flow"]["current_export_relative_path"],
        "flow/revisions/rev-0001/flow-lock.json"
    );
    assert_eq!(
        manifest_json["flow"]["revisions"][0]["apply_state"],
        "applied"
    );
    assert_eq!(
        manifest_json["flow"]["revisions"][0]["flow_lock_id"],
        lock.id()
    );
    assert_eq!(
        manifest_json["flow"]["revisions"][0]["content_hash"],
        content_hash
    );
    assert_eq!(
        manifest_json["flow"]["revisions"][0]["export_format"],
        "json"
    );
    assert_eq!(
        manifest_json["flow"]["revisions"][0]["relative_path"],
        "flow/revisions/rev-0001/flow-lock.json"
    );
    assert_eq!(
        manifest_json["artifact_paths"]["manifest"]
            .as_str()
            .unwrap(),
        manifest_path.to_string_lossy().as_ref()
    );
    assert_eq!(
        manifest_json["artifact_paths"]["flow_current"]
            .as_str()
            .unwrap(),
        revision_flow_path.to_string_lossy().as_ref()
    );
    assert_eq!(
        manifest_json["artifact_paths"]["flow_revisions"][0]
            .as_str()
            .unwrap(),
        revision_flow_path.to_string_lossy().as_ref()
    );
    assert_eq!(
        manifest_json["artifact_paths"]["flow_revision_relative_paths"][0],
        "flow/revisions/rev-0001/flow-lock.json"
    );
}

#[test]
fn activation_transcripts_are_recorded_separately_and_manifest_updates_completion() {
    let root = test_temp_dir("run-assets-activations");
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));
    let lock = flow_lock(&draft(), FlowCheckMode::Core).unwrap();
    let mut manifest = store.start_run_manifest("run-transcripts").unwrap();
    store
        .persist_flow_revision(&mut manifest, &lock, "hash:abc123", "not_required")
        .unwrap();

    let first = store
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
                },
                adapter: "tmux".to_string(),
                termination_reason: None,
            },
        )
        .unwrap();
    let second = store
        .start_activation_capture(
            &mut manifest,
            RunAssetActivationUpdate {
                activation_id: "reviewer".to_string(),
                node_id: "reviewer".to_string(),
                tmux: RunAssetTmuxTarget {
                    session_id: "host-a".to_string(),
                    window_id: "%7".to_string(),
                    window_name: "flow-a".to_string(),
                    pane_id: "%9".to_string(),
                },
                adapter: "tmux".to_string(),
                termination_reason: None,
            },
        )
        .unwrap();

    assert_ne!(first.pipe_path, second.pipe_path);
    assert_ne!(first.metadata_path, second.metadata_path);
    assert!(
        first
            .pipe_path
            .to_string_lossy()
            .contains("/activations/act-sha256-")
    );
    assert!(
        first
            .metadata_path
            .to_string_lossy()
            .contains("/activations/act-sha256-")
    );
    assert!(
        second
            .pipe_path
            .to_string_lossy()
            .contains("/activations/act-sha256-")
    );

    store
        .mark_activation_capture_acknowledged(&mut manifest, "root")
        .unwrap();
    store
        .complete_activation_capture(
            &mut manifest,
            "root",
            "contract_satisfied",
            "final transcript",
        )
        .unwrap();

    let manifest_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest.manifest_path).unwrap()).unwrap();
    let root_metadata_relative = manifest_json["activations"]["root"]["relative_paths"]["metadata"]
        .as_str()
        .unwrap();
    let metadata_json: Value = serde_json::from_str(
        &fs::read_to_string(manifest.root.join(root_metadata_relative)).unwrap(),
    )
    .unwrap();
    assert_eq!(metadata_json["run_id"], "run-transcripts");
    assert_eq!(metadata_json["activation_id"], "root");
    assert_eq!(metadata_json["node_id"], "root");
    assert_eq!(metadata_json["tmux_target"], "host-a:%7.%8");
    assert_eq!(metadata_json["adapter"], "tmux");
    assert_eq!(
        metadata_json["relative_paths"]["metadata"],
        root_metadata_relative
    );
    assert_eq!(
        metadata_json["relative_paths"]["transcript_pipe"],
        manifest_json["activations"]["root"]["relative_paths"]["transcript_pipe"]
    );
    assert_eq!(
        metadata_json["relative_paths"]["final_capture"],
        manifest_json["activations"]["root"]["relative_paths"]["final_capture"]
    );
    assert_eq!(
        manifest_json["activations"]["root"]["capture_complete"],
        true
    );
    assert_eq!(
        manifest_json["activations"]["root"]["metadata_path"]
            .as_str()
            .unwrap(),
        manifest
            .root
            .join(root_metadata_relative)
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(
        manifest_json["activations"]["root"]["relative_paths"]["metadata"],
        root_metadata_relative
    );
    assert_eq!(
        manifest_json["activations"]["root"]["termination_reason"],
        "contract_satisfied"
    );
    assert_eq!(
        fs::read_to_string(
            manifest.root.join(
                manifest_json["activations"]["root"]["relative_paths"]["final_capture"]
                    .as_str()
                    .unwrap()
            )
        )
        .unwrap(),
        "final transcript"
    );
    assert_eq!(
        manifest_json["activations"]["reviewer"]["capture_complete"],
        false
    );
    assert_eq!(manifest_json["completion"]["complete"], false);
    assert_eq!(
        manifest_json["completion"]["incomplete_tmux_activations"],
        serde_json::json!(["reviewer"])
    );
}

#[test]
fn activation_cannot_complete_until_pipe_start_is_acknowledged() {
    let root = test_temp_dir("run-assets-ack-gated-completion");
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));
    let mut manifest = store.start_run_manifest("run-ack-gate").unwrap();
    let lock = flow_lock(&draft(), FlowCheckMode::Core).unwrap();
    store
        .persist_flow_revision(&mut manifest, &lock, "hash:abc123", "not_required")
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
                },
                adapter: "tmux".to_string(),
                termination_reason: None,
            },
        )
        .unwrap();
    let starting = manifest.activations.get("root").unwrap();
    assert_eq!(starting.capture_phase, "starting");
    assert!(!starting.pipe_acknowledged);
    assert_eq!(starting.preservation_status, "starting");

    let completion = store.complete_activation_capture(&mut manifest, "root", "done", "final");

    assert!(completion.is_err());
    let still_starting = manifest.activations.get("root").unwrap();
    assert_eq!(still_starting.capture_phase, "starting");
    assert!(!still_starting.pipe_acknowledged);
    assert!(!still_starting.capture_complete);
    assert!(!manifest.completion.complete);

    store
        .mark_activation_capture_acknowledged(&mut manifest, "root")
        .unwrap();
    let capturing = manifest.activations.get("root").unwrap();
    assert_eq!(capturing.capture_phase, "capturing");
    assert!(capturing.pipe_acknowledged);

    store
        .complete_activation_capture(&mut manifest, "root", "done", "final")
        .unwrap();

    let complete = manifest.activations.get("root").unwrap();
    assert_eq!(complete.capture_phase, "complete");
    assert!(complete.pipe_acknowledged);
    assert!(complete.capture_complete);
    assert!(manifest.completion.complete);
}

#[test]
fn flow_revision_manifest_write_failure_does_not_mark_in_memory_flow_complete() {
    let root = test_temp_dir("run-assets-atomic-flow-failure");
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));
    let lock = flow_lock(&draft(), FlowCheckMode::Core).unwrap();
    let mut manifest = store.start_run_manifest("run-flow-fail").unwrap();
    fs::remove_file(&manifest.manifest_path).unwrap();
    fs::create_dir(&manifest.manifest_path).unwrap();

    let result = store.persist_flow_revision(&mut manifest, &lock, "hash:abc123", "not_required");

    assert!(result.is_err());
    assert!(!manifest.flow.complete);
    assert_eq!(manifest.flow.status, "pending");
    assert!(manifest.flow.revisions.is_empty());
    assert!(!manifest.completion.complete);
}

#[cfg(unix)]
#[test]
fn asset_store_rejects_symlink_components_and_pipe_log_destinations() {
    let root = test_temp_dir("run-assets-symlink");
    fs::create_dir_all(&root).unwrap();
    let outside = test_temp_dir("run-assets-symlink-outside");
    fs::create_dir_all(&outside).unwrap();
    let root_link = root.join("root-link");
    symlink(&outside, &root_link).unwrap();
    let store = RunAssetStore::new(RunAssetSink::Root(root_link));

    let root_result = store.start_run_manifest("run-link");

    assert!(root_result.is_err());
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));
    let mut manifest = store.start_run_manifest("run-safe").unwrap();
    let paths = store
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
                },
                adapter: "tmux".to_string(),
                termination_reason: None,
            },
        )
        .unwrap();
    fs::remove_file(&paths.pipe_path).unwrap();
    symlink(outside.join("pipe.log"), &paths.pipe_path).unwrap();

    let pipe_result = store.start_activation_capture(
        &mut manifest,
        RunAssetActivationUpdate {
            activation_id: "root".to_string(),
            node_id: "root".to_string(),
            tmux: RunAssetTmuxTarget {
                session_id: "host-a".to_string(),
                window_id: "%7".to_string(),
                window_name: "flow-a".to_string(),
                pane_id: "%8".to_string(),
            },
            adapter: "tmux".to_string(),
            termination_reason: None,
        },
    );

    assert!(pipe_result.is_err());
}

#[cfg(unix)]
#[test]
fn asset_store_rejects_fifo_transcript_without_blocking() {
    const CHILD_ROOT: &str = "HUMANIZE_FIFO_ASSET_ROOT";
    const CHILD_MANIFEST: &str = "HUMANIZE_FIFO_ASSET_MANIFEST";
    if let (Ok(root), Ok(manifest_path)) =
        (std::env::var(CHILD_ROOT), std::env::var(CHILD_MANIFEST))
    {
        let store = RunAssetStore::new(RunAssetSink::Root(PathBuf::from(root)));
        let mut manifest: RunAssetManifest =
            serde_json::from_str(&fs::read_to_string(manifest_path).unwrap()).unwrap();
        let result = store.start_activation_capture(
            &mut manifest,
            RunAssetActivationUpdate {
                activation_id: "root".to_string(),
                node_id: "root".to_string(),
                tmux: RunAssetTmuxTarget {
                    session_id: "host-a".to_string(),
                    window_id: "%7".to_string(),
                    window_name: "flow-a".to_string(),
                    pane_id: "%8".to_string(),
                },
                adapter: "tmux".to_string(),
                termination_reason: None,
            },
        );
        assert!(result.is_err());
        return;
    }

    let root = test_temp_dir("run-assets-transcript-fifo");
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));
    let mut manifest = store.start_run_manifest("run-fifo").unwrap();
    let paths = store
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
                },
                adapter: "tmux".to_string(),
                termination_reason: None,
            },
        )
        .unwrap();
    fs::remove_file(&paths.pipe_path).unwrap();
    let fifo_c = std::ffi::CString::new(paths.pipe_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: mkfifo receives a valid nul-terminated filesystem path.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let manifest_path = root.join("fifo-child-manifest.json");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "asset_store_rejects_fifo_transcript_without_blocking",
            "--nocapture",
        ])
        .env(CHILD_ROOT, &root)
        .env(CHILD_MANIFEST, &manifest_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(1) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("asset store blocked while opening a FIFO transcript");
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    assert!(status.success());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(unix)]
#[test]
fn asset_store_rejects_open_fifo_transcript_before_mutating_it() {
    let root = test_temp_dir("run-assets-open-transcript-fifo");
    let store = RunAssetStore::new(RunAssetSink::Root(root));
    let mut manifest = store.start_run_manifest("run-open-fifo").unwrap();
    let update = RunAssetActivationUpdate {
        activation_id: "root".to_string(),
        node_id: "root".to_string(),
        tmux: RunAssetTmuxTarget {
            session_id: "host-a".to_string(),
            window_id: "%7".to_string(),
            window_name: "flow-a".to_string(),
            pane_id: "%8".to_string(),
        },
        adapter: "tmux".to_string(),
        termination_reason: None,
    };
    let paths = store
        .start_activation_capture(&mut manifest, update.clone())
        .unwrap();
    fs::remove_file(&paths.pipe_path).unwrap();
    let fifo_c = std::ffi::CString::new(paths.pipe_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: mkfifo receives a valid nul-terminated filesystem path.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o644) }, 0);
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK);
    let _keepalive = options.open(&paths.pipe_path).unwrap();

    let result = store.start_activation_capture(&mut manifest, update);

    assert!(result.is_err());
    assert_eq!(
        fs::symlink_metadata(&paths.pipe_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
}

#[cfg(unix)]
#[test]
fn asset_store_creates_private_directories_and_files() {
    let root = test_temp_dir("run-assets-private");
    let store = RunAssetStore::new(RunAssetSink::Root(root.clone()));
    let lock = flow_lock(&draft(), FlowCheckMode::Core).unwrap();
    let mut manifest = store.start_run_manifest("run-private").unwrap();
    store
        .persist_flow_revision(&mut manifest, &lock, "hash:abc123", "not_required")
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
        .complete_activation_capture(&mut manifest, "root", "done", "final")
        .unwrap();

    assert_eq!(
        fs::metadata(&manifest.root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let activation = manifest.activations.get("root").unwrap();
    for path in [
        manifest.manifest_path.clone(),
        manifest.root.join("flow/revisions/rev-0001/flow-lock.json"),
        activation.metadata_path.clone(),
        activation.pipe_path.clone(),
        activation.final_capture_path.clone(),
    ] {
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "{}",
            path.display()
        );
    }
}

fn assert_fixture_matches(path: &str, manifest: RunAssetManifest) {
    let expected = serde_json::to_string_pretty(&manifest).unwrap() + "\n";
    let actual = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|err| panic!("fixture {path} should be readable: {err}"));
    assert_eq!(actual, expected);
}

fn fixture_running_manifest() -> RunAssetManifest {
    fixture_manifest(false)
}

fn fixture_completed_manifest() -> RunAssetManifest {
    fixture_manifest(true)
}

fn fixture_manifest(complete: bool) -> RunAssetManifest {
    let base_name = if complete {
        "run-assets-fixture-completed-producer"
    } else {
        "run-assets-fixture-running-producer"
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let root = test_temp_dir(&format!("{base_name}-{nonce}"));
    let store =
        RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root.clone()), 1_700_000_000_000);
    let lock = flow_lock(&draft(), FlowCheckMode::Core).unwrap();
    let mut manifest = store.start_run_manifest("worker:artifact/0").unwrap();
    store
        .persist_flow_revision(
            &mut manifest,
            &lock,
            &lock.id().replace("flk_", "fnv1a64:"),
            "not_required",
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
                },
                adapter: "tmux".to_string(),
                termination_reason: None,
            },
        )
        .unwrap();
    store
        .mark_activation_capture_acknowledged(&mut manifest, "root")
        .unwrap();
    if complete {
        store
            .complete_activation_capture(
                &mut manifest,
                "root",
                "contract_satisfied",
                "final capture\n",
            )
            .unwrap();
        store
            .mark_activation_resource_cleanup(&mut manifest, "root", "complete", None)
            .unwrap();
    }
    normalize_fixture_root(manifest)
}

fn normalize_fixture_root(mut manifest: RunAssetManifest) -> RunAssetManifest {
    let old_root = manifest.root.clone();
    let new_root = PathBuf::from("/tmp/humanize-fixtures").join(&manifest.storage.run_directory);
    manifest.root = new_root.clone();
    manifest.manifest_path = normalize_path(&old_root, &new_root, &manifest.manifest_path);
    manifest.artifact_paths.manifest =
        normalize_path(&old_root, &new_root, &manifest.artifact_paths.manifest);
    manifest.flow.current_export_path = manifest
        .flow
        .current_export_path
        .as_ref()
        .map(|path| normalize_path(&old_root, &new_root, path));
    for revision in &mut manifest.flow.revisions {
        revision.export_path = normalize_path(&old_root, &new_root, &revision.export_path);
    }
    manifest.artifact_paths.flow_current = manifest
        .artifact_paths
        .flow_current
        .as_ref()
        .map(|path| normalize_path(&old_root, &new_root, path));
    manifest.artifact_paths.flow_revisions = manifest
        .artifact_paths
        .flow_revisions
        .iter()
        .map(|path| normalize_path(&old_root, &new_root, path))
        .collect();
    for activation in manifest.activations.values_mut() {
        activation.metadata_path = normalize_path(&old_root, &new_root, &activation.metadata_path);
        activation.pipe_path = normalize_path(&old_root, &new_root, &activation.pipe_path);
        activation.final_capture_path =
            normalize_path(&old_root, &new_root, &activation.final_capture_path);
    }
    manifest
}

fn normalize_path(old_root: &Path, new_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(old_root)
        .map(|relative| new_root.join(relative))
        .unwrap_or_else(|_| path.to_path_buf())
}
