use std::collections::BTreeMap;

use humanize_plugin::kernel::{
    Artifact, Board, BoardPatch, BoardPatchError, BoardValue, CompletionRule, Contract,
    ContractPermit, ContractProduction, ContractRequirement, Event, Node, Route,
    kernel_primitive_names,
};

#[test]
fn kernel_primitive_names_are_exactly_the_authoring_kernel() {
    let primitives = kernel_primitive_names();

    assert_eq!(
        primitives,
        ["Node", "Contract", "Artifact", "Board", "Route", "Event"]
    );
    assert!(!primitives.contains(&"Effect"));
    assert!(!primitives.contains(&"Activation"));
    assert!(!primitives.contains(&"NodeActivation"));
}

#[test]
fn artifact_creation_keeps_typed_payload_immutable_and_fingerprinted() {
    let payload = BTreeMap::from([
        ("command".to_string(), "cargo test".to_string()),
        ("status".to_string(), "passed".to_string()),
    ]);

    let artifact = Artifact::new(
        "artifact:test-result",
        "test result",
        "test.result.v1",
        payload.clone(),
    );
    let duplicate = Artifact::new(
        "artifact:test-result",
        "test result",
        "test.result.v1",
        payload.clone(),
    );
    let changed = Artifact::new(
        "artifact:test-result",
        "test result",
        "test.result.v1",
        BTreeMap::from([("status".to_string(), "failed".to_string())]),
    );

    assert_eq!(artifact.id(), "artifact:test-result");
    assert_eq!(artifact.name(), "test result");
    assert_eq!(artifact.schema(), "test.result.v1");
    assert_eq!(artifact.payload(), &payload);
    assert_eq!(artifact.fingerprint(), duplicate.fingerprint());
    assert_ne!(artifact.fingerprint(), changed.fingerprint());

    let mut detached_payload = artifact.payload().clone();
    detached_payload.insert("status".to_string(), "mutated outside".to_string());
    assert_eq!(
        artifact.payload().get("status"),
        Some(&"passed".to_string())
    );
}

#[test]
fn board_patch_increments_version_and_rejects_stale_writes() {
    let mut board = Board::new("board:run");

    let first = BoardPatch::new(0)
        .set(
            "last_artifact",
            BoardValue::Text("artifact:test-result".to_string()),
        )
        .set("passed", BoardValue::Bool(true));

    let first_version = board.apply(first).expect("first patch should apply");

    assert_eq!(first_version, 1);
    assert_eq!(board.version(), 1);
    assert_eq!(
        board.get("last_artifact"),
        Some(&BoardValue::Text("artifact:test-result".to_string()))
    );
    assert_eq!(board.get("passed"), Some(&BoardValue::Bool(true)));

    let stale = BoardPatch::new(0).set("passed", BoardValue::Bool(false));
    let err = board.apply(stale).expect_err("stale patch should conflict");

    assert_eq!(
        err,
        BoardPatchError::VersionConflict {
            expected: 0,
            actual: 1,
        }
    );
    assert_eq!(board.version(), 1);
    assert_eq!(board.get("passed"), Some(&BoardValue::Bool(true)));
}

#[test]
fn contract_describes_local_inputs_outputs_permissions_and_completion() {
    let contract = Contract::new("contract:test-node")
        .require(ContractRequirement::artifact_schema("source.diff.v1"))
        .produce(ContractProduction::artifact_schema("test.result.v1"))
        .permit(ContractPermit::RecordEffect)
        .with_completion(CompletionRule::AllProducedArtifactsRecorded);

    let node = Node::new("node:test", "Run tests", contract.id());

    assert_eq!(node.contract_id(), "contract:test-node");
    assert_eq!(
        contract.requires(),
        &[ContractRequirement::artifact_schema("source.diff.v1")]
    );
    assert_eq!(
        contract.produces(),
        &[ContractProduction::artifact_schema("test.result.v1")]
    );
    assert_eq!(contract.permits(), &[ContractPermit::RecordEffect]);
    assert_eq!(
        contract.completion(),
        &CompletionRule::AllProducedArtifactsRecorded
    );
}

#[test]
fn route_keeps_predicate_text_and_optional_for_each_expression_only() {
    let single = Route::new("route:single", "node:test", "node:review")
        .when("artifact.schema == 'test.result.v1' && artifact.status == 'passed'");
    let fanout = Route::new("route:fanout", "node:test", "node:review")
        .when("artifact.schema == 'test.result.v1'")
        .for_each_artifact("artifact.test_results");

    assert_eq!(
        single.predicate(),
        "artifact.schema == 'test.result.v1' && artifact.status == 'passed'"
    );
    assert_eq!(single.for_each(), None);
    assert_eq!(fanout.for_each(), Some("artifact.test_results"));
}

#[test]
fn events_are_append_only_facts_that_can_record_effects_and_flow_applications() {
    let artifact = Artifact::new(
        "artifact:test-result",
        "test result",
        "test.result.v1",
        BTreeMap::from([("status".to_string(), "passed".to_string())]),
    );

    let events = [
        Event::NodeStarted {
            node_id: "node:test".to_string(),
        },
        Event::ArtifactCreated {
            artifact: artifact.clone(),
        },
        Event::EffectRecorded {
            node_id: "node:test".to_string(),
            effect_key: "process.exit".to_string(),
            fields: BTreeMap::from([("code".to_string(), "0".to_string())]),
        },
        Event::FlowApplied {
            run_id: "run:test".to_string(),
            lock_id: "lock:child".to_string(),
            content_hash: "sha256:test".to_string(),
            mode: "future_activations".to_string(),
        },
    ];

    assert_eq!(events.len(), 4);
    assert!(matches!(events[0], Event::NodeStarted { .. }));
    assert!(matches!(events[1], Event::ArtifactCreated { .. }));
    assert!(matches!(events[2], Event::EffectRecorded { .. }));
    assert!(matches!(
        &events[3],
        Event::FlowApplied {
            run_id,
            lock_id,
            content_hash,
            mode,
        } if run_id == "run:test"
            && lock_id == "lock:child"
            && content_hash == "sha256:test"
            && mode == "future_activations"
    ));
}
