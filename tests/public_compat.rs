use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::PathBuf;

use humanize_plugin::flow::{
    ContractArtifact, ContractCompletion, FlowContract, FlowDraft, FlowNode, FlowPolicies,
};
use humanize_plugin::mcp::serve_stdio;
use humanize_plugin::run_assets::{
    RunAssetArtifactPaths, RunAssetCompletion, RunAssetFlow, RunAssetManifest, RunAssetProtocol,
    RunAssetSink, RunAssetStorage, RunAssetStore,
};
use humanize_plugin::view::RunSnapshot;

#[test]
fn flow_contract_old_struct_literal_still_compiles() {
    let contract = FlowContract {
        id: "contract.root".to_string(),
        completion: Some(ContractCompletion::AllArtifacts),
        artifacts: vec![ContractArtifact {
            id: "report".to_string(),
            schema_resource_id: None,
        }],
    };

    assert_eq!(contract.id, "contract.root");
}

#[test]
fn flow_draft_and_node_old_struct_literals_still_compile() {
    let node = FlowNode {
        id: "root".to_string(),
        contract_id: None,
        action: None,
        write_scopes: Vec::new(),
        extensions: Vec::new(),
    };
    let draft = FlowDraft {
        nodes: vec![node],
        contracts: Vec::new(),
        routes: Vec::new(),
        resources: Vec::new(),
        imports: Vec::new(),
        policies: FlowPolicies::default(),
        extensions: Vec::new(),
    };

    assert_eq!(draft.nodes[0].id, "root");
}

#[test]
fn run_asset_manifest_old_struct_literal_still_compiles() {
    let root = PathBuf::from("/tmp/humanize-compat/run");
    let manifest = RunAssetManifest {
        version: 1,
        run_id: "run-a".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        sink: "root".to_string(),
        root: root.clone(),
        manifest_path: root.join("manifest.json"),
        storage: RunAssetStorage {
            raw_run_id: "run-a".to_string(),
            run_directory: "run-a".to_string(),
            run_relative_path: "run-a".to_string(),
        },
        protocol: RunAssetProtocol {
            mcp_protocol_version: "2024-11-05".to_string(),
            package_name: "humanize-plugin".to_string(),
            package_version: "0.1.0".to_string(),
        },
        flow: RunAssetFlow {
            main_flow: true,
            status: "pending".to_string(),
            complete: false,
            current_revision_id: None,
            current_export_path: None,
            current_export_relative_path: None,
            revisions: Vec::new(),
        },
        artifact_paths: RunAssetArtifactPaths {
            manifest: root.join("manifest.json"),
            manifest_relative_path: "manifest.json".to_string(),
            flow_current: None,
            flow_current_relative_path: None,
            flow_revisions: Vec::new(),
            flow_revision_relative_paths: Vec::new(),
        },
        activations: BTreeMap::new(),
        preservation_errors: Vec::new(),
        preservation_blocked: false,
        completion: RunAssetCompletion::default(),
    };

    assert_eq!(manifest.run_id, "run-a");
}

#[test]
#[allow(deprecated)]
fn deprecated_sforge_patch_dir_sink_alias_still_compiles_without_runtime_default_coupling() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join("compat-sforge-sink");
    let store = RunAssetStore::new(RunAssetSink::SforgePatchDir(root.clone()));

    let run_root = store.run_root("run-a").unwrap();

    assert!(run_root.starts_with(root));
}

#[test]
fn serve_stdio_accepts_borrowed_reader_and_writer() {
    let mut reader = Cursor::new(Vec::<u8>::new());
    let mut writer = Vec::<u8>::new();

    serve_stdio(&mut reader, &mut writer).unwrap();

    assert!(writer.is_empty());
}

#[test]
fn run_snapshot_old_struct_literal_still_compiles() {
    let snapshot = RunSnapshot {
        run_id: "run-a".to_string(),
        run_status: "running".to_string(),
        driver_mode: "event_driven_mcp".to_string(),
        driver_mode_detail: "tool calls advance runtime".to_string(),
        activation_count: 0,
        artifact_count: 0,
        effect_count: 0,
        board_version: 0,
        message_count: 0,
        missing_stop_contract_count: 0,
        activation_ids: Vec::new(),
        activations: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        latest_artifact_by_slot_index: BTreeMap::new(),
        effects: BTreeMap::new(),
        board: BTreeMap::new(),
        flow_lock_mode: None,
        flow_lock_id: None,
        content_hash: None,
        flow_review_status: None,
        flow_export_document: None,
        latest_flow_lock_application: None,
        flow_lock_applications: BTreeMap::new(),
        missing_stop_contracts: BTreeMap::new(),
        runtime_budgets: Vec::new(),
        pane_mappings: Vec::new(),
        event_count: 0,
        event_timeline: Vec::new(),
        last_decision: None,
        stop_decisions: Vec::new(),
        machine_inputs: Vec::new(),
        actuation_warnings: Vec::new(),
        waiting_human: Vec::new(),
        why: None,
    };

    assert_eq!(snapshot.run_id, "run-a");
}
