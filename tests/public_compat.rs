use std::collections::BTreeMap;
use std::io::Cursor;

use humanize_plugin::flow::{ContractArtifact, ContractCompletion, FlowContract};
use humanize_plugin::mcp::serve_stdio;
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
        why: None,
    };

    assert_eq!(snapshot.run_id, "run-a");
}
