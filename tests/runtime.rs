use humanize_plugin::runtime::{
    ActivationStatus, BoardPatch, EventPayload, FlowLockMode, LocalEventStore, NodeSpec, Runtime,
    RuntimeState, StopContract, StopValidationError,
};

fn node_with_contract(
    id: &str,
    required_artifacts: &[&str],
    required_effects: &[&str],
) -> NodeSpec {
    NodeSpec::new(id).with_stop_contract(StopContract::new(
        required_artifacts.iter().copied(),
        required_effects.iter().copied(),
    ))
}

fn activation_key(run_id: &str, activation_id: &str) -> (String, String) {
    (run_id.to_owned(), activation_id.to_owned())
}

fn slot_key(run_id: &str, artifact_key: &str) -> (String, String) {
    (run_id.to_owned(), artifact_key.to_owned())
}

fn effect_key(run_id: &str, activation_id: &str, effect_key: &str) -> (String, String, String) {
    (
        run_id.to_owned(),
        activation_id.to_owned(),
        effect_key.to_owned(),
    )
}

#[test]
fn local_event_store_appends_and_replays_events_in_order() {
    let mut store = LocalEventStore::default();

    let first = store.append(EventPayload::RunStarted {
        run_id: "run-a".into(),
    });
    let second = store.append(EventPayload::EffectRecorded {
        run_id: "run-a".into(),
        activation_id: "act-a".into(),
        effect_key: "log".into(),
        payload: "hello".into(),
    });

    assert_eq!(first.sequence, 1);
    assert_eq!(second.sequence, 2);
    assert_eq!(
        store
            .replay()
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(matches!(
        store.replay()[0].payload,
        EventPayload::RunStarted { ref run_id } if run_id == "run-a"
    ));
}

#[test]
fn start_run_records_run_and_initial_node_activations() {
    let nodes = vec![NodeSpec::new("ingest"), NodeSpec::new("review")];
    let mut runtime = Runtime::default();

    let activation_ids = runtime.start_run("run-a", nodes).unwrap();

    assert_eq!(activation_ids, vec!["ingest", "review"]);
    assert_eq!(runtime.events().len(), 3);
    assert_eq!(
        runtime
            .state()
            .activations
            .get(&activation_key("run-a", "ingest"))
            .map(|activation| activation.status),
        Some(ActivationStatus::Active)
    );
    assert_eq!(
        runtime
            .state()
            .activations
            .get(&activation_key("run-a", "review"))
            .map(|activation| activation.node_id.as_str()),
        Some("review")
    );
}

#[test]
fn start_run_allows_same_activation_id_in_distinct_runs() {
    let mut runtime = Runtime::default();

    let first = runtime
        .start_run("run-a", vec![NodeSpec::new("root")])
        .unwrap();
    let second = runtime
        .start_run("run-b", vec![NodeSpec::new("root")])
        .unwrap();

    assert_eq!(first, vec!["root"]);
    assert_eq!(second, vec!["root"]);
}

#[test]
fn duplicate_start_run_does_not_append_partial_state() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-a", vec![NodeSpec::new("root")])
        .unwrap();
    let event_count = runtime.events().len();

    let err = runtime
        .start_run("run-a", vec![NodeSpec::new("root")])
        .unwrap_err();

    assert_eq!(err.to_string(), "duplicate run: run-a");
    assert_eq!(runtime.events().len(), event_count);
}

#[test]
fn runtime_rejects_cross_run_activation_mutation() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-a", vec![NodeSpec::new("only-a")])
        .unwrap();
    runtime
        .start_run("run-b", vec![NodeSpec::new("only-b")])
        .unwrap();

    let delivered = runtime
        .deliver_artifact("run-b", "only-a", "brief", "wrong run")
        .unwrap_err();
    let patched = runtime
        .patch_board("run-b", "only-a", BoardPatch::new("summary", "wrong run"))
        .unwrap_err();
    let effect = runtime
        .record_effect("run-b", "only-a", "shell", "wrong run")
        .unwrap_err();

    assert_eq!(
        delivered.to_string(),
        "activation not found in run run-b: only-a"
    );
    assert_eq!(
        patched.to_string(),
        "activation not found in run run-b: only-a"
    );
    assert_eq!(
        effect.to_string(),
        "activation not found in run run-b: only-a"
    );
}

#[test]
fn deliver_artifact_preserves_immutable_records_and_indexes_latest_slot() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-a", vec![NodeSpec::new("ingest")])
        .unwrap();

    let first_artifact_id = runtime
        .deliver_artifact("run-a", "ingest", "brief", "first draft")
        .unwrap();
    let second_artifact_id = runtime
        .deliver_artifact("run-a", "ingest", "brief", "second draft")
        .unwrap();

    assert_ne!(first_artifact_id, second_artifact_id);
    let first_record = runtime
        .state()
        .artifact_records
        .get(&first_artifact_id)
        .expect("first artifact record should remain addressable");
    let second_record = runtime
        .state()
        .artifact_records
        .get(&second_artifact_id)
        .expect("second artifact record should be addressable");

    assert_eq!(first_record.artifact_key, "brief");
    assert_eq!(first_record.payload, "first draft");
    assert_eq!(second_record.payload, "second draft");
    assert_ne!(first_record.content_hash, second_record.content_hash);
    assert_eq!(
        runtime
            .state()
            .latest_artifact_by_slot_index
            .get(&slot_key("run-a", "brief")),
        Some(&second_artifact_id)
    );
    assert!(matches!(
        runtime.events().last().map(|event| &event.payload),
        Some(EventPayload::ArtifactDelivered {
            artifact_id,
            artifact_key,
            content_hash,
            payload,
            ..
        }) if artifact_id == &second_artifact_id
            && artifact_key == "brief"
            && !content_hash.is_empty()
            && payload == "second draft"
    ));
}

#[test]
fn patch_board_detects_version_conflicts_and_records_next_version() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-a", vec![NodeSpec::new("ingest")])
        .unwrap();

    let version = runtime
        .patch_board(
            "run-a",
            "ingest",
            BoardPatch::new("summary", "ready").expect_version(0),
        )
        .unwrap();
    let conflict = runtime
        .patch_board(
            "run-a",
            "ingest",
            BoardPatch::new("summary", "stale").expect_version(0),
        )
        .unwrap_err();

    assert_eq!(version, 1);
    assert_eq!(
        conflict.to_string(),
        "board version conflict: expected 0, actual 1"
    );
    assert_eq!(runtime.state().board_version, 1);
    assert_eq!(
        runtime.state().board.get("summary").map(String::as_str),
        Some("ready")
    );
}

#[test]
fn runtime_state_can_be_replayed_from_the_event_log() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-a", vec![NodeSpec::new("ingest")])
        .unwrap();
    runtime
        .deliver_artifact("run-a", "ingest", "brief", "ready")
        .unwrap();
    runtime
        .patch_board(
            "run-a",
            "ingest",
            BoardPatch::new("summary", "ready").expect_version(0),
        )
        .unwrap();
    runtime
        .record_effect("run-a", "ingest", "shell", "ok")
        .unwrap();

    let replayed = RuntimeState::from_events(runtime.events());

    assert_eq!(replayed, *runtime.state());
}

#[test]
fn record_effect_appends_event_payload_without_a_new_primitive() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-a", vec![NodeSpec::new("worker")])
        .unwrap();

    runtime
        .record_effect("run-a", "worker", "shell", "cargo test")
        .unwrap();

    assert_eq!(
        runtime
            .state()
            .effects
            .get(&effect_key("run-a", "worker", "shell"))
            .map(String::as_str),
        Some("cargo test")
    );
    assert!(matches!(
        runtime.events().last().map(|event| &event.payload),
        Some(EventPayload::EffectRecorded { effect_key, payload, .. })
            if effect_key == "shell" && payload == "cargo test"
    ));
}

#[test]
fn activate_node_creates_runtime_activation_without_mutating_templates() {
    let template = NodeSpec::new("map").with_for_each("items");
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-a", vec![NodeSpec::new("root")])
        .unwrap();

    let activation_id = runtime
        .activate_node("run-a", &template, Some("items/1"))
        .unwrap();

    assert_eq!(activation_id, "map:items/1");
    assert_eq!(template.for_each_key(), Some("items"));
    assert_eq!(
        runtime
            .state()
            .activations
            .get(&activation_key("run-a", "map:items/1"))
            .map(|activation| activation.node_id.as_str()),
        Some("map")
    );
}

#[test]
fn fanout_activation_uses_artifact_data_and_stable_keys() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-a", vec![NodeSpec::new("root")])
        .unwrap();
    runtime
        .deliver_artifact("run-a", "root", "items", "alpha\nbeta\nalpha")
        .unwrap();

    let activation_ids = runtime
        .fanout_from_artifact(
            "run-a",
            &NodeSpec::new("process").with_for_each("items"),
            "items",
        )
        .unwrap();

    assert_eq!(
        activation_ids,
        vec!["process:items/0", "process:items/1", "process:items/2"]
    );
    assert_eq!(
        runtime
            .state()
            .activations
            .get(&activation_key("run-a", "process:items/1"))
            .map(|activation| activation.context.get("item").map(String::as_str)),
        Some(Some("beta"))
    );
}

#[test]
fn fanout_duplicate_activation_rejects_without_partial_append() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-a", vec![NodeSpec::new("root")])
        .unwrap();
    runtime
        .deliver_artifact("run-a", "root", "items", "alpha\nbeta")
        .unwrap();
    runtime
        .activate_node("run-a", &NodeSpec::new("process"), Some("items/1"))
        .unwrap();
    let event_count = runtime.events().len();

    let err = runtime
        .fanout_from_artifact(
            "run-a",
            &NodeSpec::new("process").with_for_each("items"),
            "items",
        )
        .unwrap_err();

    assert_eq!(err.to_string(), "duplicate activation: process:items/1");
    assert_eq!(runtime.events().len(), event_count);
    assert!(
        !runtime
            .state()
            .activations
            .contains_key(&activation_key("run-a", "process:items/0"))
    );
}

#[test]
fn validate_stop_only_checks_the_local_activation_contract() {
    let mut runtime = Runtime::default();
    runtime
        .start_run(
            "run-a",
            vec![
                node_with_contract("a", &["brief"], &["shell"]),
                node_with_contract("b", &["brief"], &["approval"]),
            ],
        )
        .unwrap();
    runtime
        .deliver_artifact("run-a", "a", "brief", "ready")
        .unwrap();
    runtime.record_effect("run-a", "a", "shell", "ok").unwrap();

    assert_eq!(runtime.validate_stop("run-a", "a"), Ok(()));
    assert_eq!(
        runtime.validate_stop("run-a", "b"),
        Err(StopValidationError::MissingArtifact {
            activation_id: "b".into(),
            artifact_key: "brief".into(),
        })
    );
}

#[test]
fn validate_stop_requires_run_id_for_reused_activation_ids() {
    let mut runtime = Runtime::default();
    runtime
        .start_run(
            "run-a",
            vec![node_with_contract("shared", &["brief"], &["shell"])],
        )
        .unwrap();
    runtime
        .start_run(
            "run-b",
            vec![node_with_contract("shared", &["brief"], &["shell"])],
        )
        .unwrap();
    runtime
        .deliver_artifact("run-a", "shared", "brief", "ready")
        .unwrap();
    runtime
        .record_effect("run-a", "shared", "shell", "ok")
        .unwrap();

    assert_eq!(runtime.validate_stop("run-a", "shared"), Ok(()));
    assert_eq!(
        runtime.validate_stop("run-b", "shared"),
        Err(StopValidationError::MissingArtifact {
            activation_id: "shared".into(),
            artifact_key: "brief".into(),
        })
    );
}

#[test]
fn apply_flow_lock_records_mode_for_future_activation_policy() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-a", vec![NodeSpec::new("root")])
        .unwrap();

    runtime
        .apply_flow_lock(
            "run-a",
            FlowLockMode::FutureActivations,
            "lock-a",
            "sha256:first",
        )
        .unwrap();
    let root_mode = runtime
        .state()
        .activations
        .get(&activation_key("run-a", "root"))
        .unwrap()
        .flow_lock_mode;
    let later = runtime
        .activate_node("run-a", &NodeSpec::new("later"), None)
        .unwrap();
    runtime
        .apply_flow_lock(
            "run-a",
            FlowLockMode::CheckpointRestart,
            "lock-b",
            "sha256:second",
        )
        .unwrap();
    let checkpointed = runtime
        .activate_node("run-a", &NodeSpec::new("checkpointed"), None)
        .unwrap();

    assert_eq!(root_mode, None);
    assert_eq!(
        runtime
            .state()
            .activations
            .get(&activation_key("run-a", &later))
            .unwrap()
            .flow_lock_mode,
        Some(FlowLockMode::FutureActivations)
    );
    assert_eq!(
        runtime
            .state()
            .activations
            .get(&activation_key("run-a", &checkpointed))
            .unwrap()
            .flow_lock_mode,
        Some(FlowLockMode::CheckpointRestart)
    );
    assert!(matches!(
        runtime.events().last().map(|event| &event.payload),
        Some(EventPayload::NodeActivated { flow_lock_mode, .. })
            if *flow_lock_mode == Some(FlowLockMode::CheckpointRestart)
    ));

    assert_eq!(
        runtime
            .state()
            .latest_flow_lock_application_index
            .as_deref(),
        Some("flow-lock-application:5")
    );
    let first_application = runtime
        .state()
        .flow_lock_applications
        .get("flow-lock-application:3")
        .expect("first flow lock application should remain addressable");
    let second_application = runtime
        .state()
        .flow_lock_applications
        .get("flow-lock-application:5")
        .expect("second flow lock application should be addressable");

    assert_eq!(first_application.lock_id, "lock-a");
    assert_eq!(first_application.content_hash, "sha256:first");
    assert_eq!(first_application.event_sequence, 3);
    assert_eq!(second_application.lock_id, "lock-b");
    assert_eq!(second_application.content_hash, "sha256:second");
    assert!(matches!(
        runtime
            .events()
            .iter()
            .find(|event| event.sequence == 5)
            .map(|event| &event.payload),
        Some(EventPayload::FlowApplied {
            run_id,
            mode,
            lock_id,
            content_hash,
        }) if run_id == "run-a"
            && *mode == FlowLockMode::CheckpointRestart
            && lock_id == "lock-b"
            && content_hash == "sha256:second"
    ));
}
