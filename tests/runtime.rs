use humanize_plugin::{
    flow::{
        ContractArtifact, ContractCompletion, FlowCheckMode, FlowContract, FlowDraft, FlowLock,
        FlowNode, FlowResource, FlowRoute, ResourceKind, flow_lock,
    },
    runtime::{
        ActivationStatus, BoardPatch, ControlCommand, DriverState, DriverTickInput, EventKind,
        EventPayload, EventStrength, FlowLockMode, FlowUpdateStatus, LocalEventStore, LoopBudget,
        NodeSpec, RunCompletionMode, RunStatus, Runtime, RuntimeState, StopContract,
        StopDecisionKind, StopObservation, StopValidationError, TickBudget, preview_flow_routes,
    },
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

fn route_preview_lock(routes: Vec<FlowRoute>) -> FlowLock {
    let mut node_ids = vec!["root".to_string()];
    for route in &routes {
        if !node_ids.contains(&route.activate) {
            node_ids.push(route.activate.clone());
        }
    }

    flow_lock(
        &FlowDraft {
            nodes: node_ids
                .into_iter()
                .map(|id| FlowNode {
                    id,
                    ..FlowNode::default()
                })
                .collect(),
            resources: vec![FlowResource {
                id: "readme.main".into(),
                kind: ResourceKind::Readme,
                source: "inline:Preview local routes.".into(),
            }],
            routes,
            ..FlowDraft::default()
        },
        FlowCheckMode::Core,
    )
    .unwrap()
}

fn route_lock_with_finish_contract() -> FlowLock {
    flow_lock(
        &FlowDraft {
            nodes: vec![
                FlowNode {
                    id: "root".into(),
                    ..FlowNode::default()
                },
                FlowNode {
                    id: "finish".into(),
                    contract_id: Some("contract.finish".into()),
                    ..FlowNode::default()
                },
            ],
            contracts: vec![FlowContract {
                id: "contract.finish".into(),
                completion: Some(ContractCompletion::AllArtifacts),
                artifacts: vec![ContractArtifact {
                    id: "summary".into(),
                    schema_resource_id: Some("schema.summary".into()),
                }],
            }],
            resources: vec![
                FlowResource {
                    id: "readme.main".into(),
                    kind: ResourceKind::Readme,
                    source: "inline:Preview local routes.".into(),
                },
                FlowResource {
                    id: "schema.summary".into(),
                    kind: ResourceKind::Schema,
                    source: "inline:summary".into(),
                },
            ],
            routes: vec![FlowRoute {
                predicate: "exists(artifact.ready)".into(),
                for_each: None,
                activate: "finish".into(),
            }],
            ..FlowDraft::default()
        },
        FlowCheckMode::Core,
    )
    .unwrap()
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
        Some(ActivationStatus::Pending)
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
fn events_have_fact_envelope_and_legacy_payloads_replay() {
    let mut store = LocalEventStore::default();

    let started = store.append(EventPayload::RunStarted {
        run_id: "run-envelope".into(),
    });
    let observed = store.append(EventPayload::StopObserved {
        run_id: "run-envelope".into(),
        activation_id: "review".into(),
        observation: StopObservation::new("pane closed"),
    });

    assert_eq!(started.sequence, 1);
    assert_eq!(started.source.run_id.as_deref(), Some("run-envelope"));
    assert_eq!(started.kind, EventKind::RunStarted);
    assert_eq!(started.strength, EventStrength::Applied);
    assert_eq!(started.actor.as_deref(), Some("runtime"));
    assert_eq!(started.correlation, None);
    assert_eq!(observed.sequence, 2);
    assert_eq!(observed.source.activation_id.as_deref(), Some("review"));
    assert_eq!(observed.kind, EventKind::StopObserved);
    assert_eq!(observed.strength, EventStrength::Observed);

    let replayed = RuntimeState::from_events(store.replay());

    assert!(replayed.runs.contains("run-envelope"));
    assert_eq!(
        replayed.stop_observations.get("run-envelope/review/2"),
        Some(&StopObservation::new("pane closed"))
    );
}

#[test]
fn driver_denies_stop_until_limit_then_blocks_and_completes_after_requirements_exist() {
    let mut driver = DriverState::default();
    driver
        .runtime_mut()
        .start_run(
            "run-stop",
            vec![node_with_contract("review", &["summary"], &["shell"])],
        )
        .unwrap();

    let first = driver.tick(
        DriverTickInput::default()
            .with_budget(TickBudget {
                stop_validation_attempt_limit: 2,
                ..TickBudget::default()
            })
            .with_stop_observation("run-stop", "review", StopObservation::new("pane exited")),
    );
    let second = driver.tick(
        DriverTickInput::default()
            .with_budget(TickBudget {
                stop_validation_attempt_limit: 2,
                ..TickBudget::default()
            })
            .with_stop_observation("run-stop", "review", StopObservation::new("pane exited")),
    );

    assert_eq!(first.stop_decisions[0].kind, StopDecisionKind::Deny);
    assert_eq!(first.stop_decisions[0].attempt, 1);
    assert_eq!(second.stop_decisions[0].kind, StopDecisionKind::Block);
    assert_eq!(
        driver
            .runtime()
            .state()
            .activations
            .get(&activation_key("run-stop", "review"))
            .map(|activation| activation.status),
        Some(ActivationStatus::Blocked)
    );

    driver
        .runtime_mut()
        .deliver_artifact("run-stop", "review", "summary", "ready")
        .unwrap();
    driver
        .runtime_mut()
        .record_effect("run-stop", "review", "shell", "ok")
        .unwrap();
    let completed = driver.tick(
        DriverTickInput::default()
            .with_budget(TickBudget {
                stop_validation_attempt_limit: 2,
                ..TickBudget::default()
            })
            .with_stop_observation("run-stop", "review", StopObservation::new("pane exited")),
    );

    assert_eq!(completed.stop_decisions[0].kind, StopDecisionKind::Allow);
    assert_eq!(
        driver
            .runtime()
            .state()
            .activations
            .get(&activation_key("run-stop", "review"))
            .map(|activation| activation.status),
        Some(ActivationStatus::Completed)
    );
    assert_eq!(
        driver.runtime().state().run_status("run-stop"),
        Some(RunStatus::Completed)
    );
    assert!(matches!(
        driver.runtime().events().last().map(|event| &event.payload),
        Some(EventPayload::RunStatusChanged { status, .. }) if *status == RunStatus::Completed
    ));
}

#[test]
fn flow_updates_bind_new_activations_without_rewriting_existing_contracts() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-flow", vec![NodeSpec::new("root")])
        .unwrap();

    runtime
        .apply_flow_lock(
            "run-flow",
            FlowLockMode::FutureActivations,
            "lock-a",
            "hash-a",
        )
        .unwrap();
    let first = runtime
        .activate_node("run-flow", &NodeSpec::new("first"), None)
        .unwrap();
    runtime
        .apply_flow_lock(
            "run-flow",
            FlowLockMode::CheckpointRestart,
            "lock-b",
            "hash-b",
        )
        .unwrap();
    let second = runtime
        .activate_node("run-flow", &NodeSpec::new("second"), None)
        .unwrap();

    let flow_statuses = runtime
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::FlowUpdate {
                status, lock_id, ..
            } => Some((*status, lock_id.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        flow_statuses,
        vec![
            (FlowUpdateStatus::Proposed, "lock-a"),
            (FlowUpdateStatus::Checked, "lock-a"),
            (FlowUpdateStatus::Applied, "lock-a"),
            (FlowUpdateStatus::Proposed, "lock-b"),
            (FlowUpdateStatus::Checked, "lock-b"),
            (FlowUpdateStatus::Applied, "lock-b"),
        ]
    );
    assert_eq!(
        runtime
            .state()
            .activations
            .get(&activation_key("run-flow", "root"))
            .and_then(|activation| activation.flow_lock_id.as_deref()),
        None
    );
    assert_eq!(
        runtime
            .state()
            .activations
            .get(&activation_key("run-flow", &first))
            .and_then(|activation| activation.flow_lock_id.as_deref()),
        Some("lock-a")
    );
    assert_eq!(
        runtime
            .state()
            .activations
            .get(&activation_key("run-flow", &first))
            .and_then(|activation| activation.contract_hash.as_deref()),
        Some("hash-a")
    );
    assert_eq!(
        runtime
            .state()
            .activations
            .get(&activation_key("run-flow", &second))
            .and_then(|activation| activation.flow_lock_id.as_deref()),
        Some("lock-b")
    );
}

#[test]
fn driver_tick_runs_ordered_pipeline_and_reports_quiescent_for_continuous_runs() {
    let mut driver = DriverState::default();

    let report = driver.tick(
        DriverTickInput::default()
            .with_loop_budget(LoopBudget {
                tick_limit: 8,
                action_limit: 8,
            })
            .with_completion_mode(RunCompletionMode::Continuous)
            .with_control(ControlCommand::StartRun {
                run_id: "run-loop".into(),
                nodes: vec![NodeSpec::new("root")],
            })
            .with_stop_observation("run-loop", "root", StopObservation::new("pane exited")),
    );

    assert_eq!(
        report.pipeline,
        vec![
            "Replay",
            "Handle Control",
            "Observe",
            "Validate",
            "Route",
            "Actuate",
            "Complete",
            "Render",
        ]
    );
    assert_eq!(
        report.render.run_statuses.get("run-loop").copied(),
        Some(RunStatus::Quiescent)
    );
    assert_eq!(
        driver
            .runtime()
            .state()
            .activations
            .get(&activation_key("run-loop", "root"))
            .map(|activation| activation.status),
        Some(ActivationStatus::Completed)
    );
}

#[test]
fn driver_tick_repeats_route_actuate_and_complete_until_quiescent() {
    let lock = route_preview_lock(vec![FlowRoute {
        predicate: "exists(artifact.ready)".into(),
        for_each: None,
        activate: "finish".into(),
    }]);
    let lock_id = lock.id().to_string();
    let mut driver = DriverState::default();
    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::StartRun {
            run_id: "run-auto-route".into(),
            nodes: vec![NodeSpec::new("root")],
        }),
    );
    driver
        .runtime_mut()
        .apply_flow_lock(
            "run-auto-route",
            FlowLockMode::FutureActivations,
            &lock_id,
            "hash-route",
        )
        .unwrap();
    driver
        .runtime_mut()
        .deliver_artifact("run-auto-route", "root", "ready", "done")
        .unwrap();

    let report = driver.tick(
        DriverTickInput::default()
            .with_loop_budget(LoopBudget {
                tick_limit: 4,
                action_limit: 16,
            })
            .with_route_lock(lock)
            .with_stop_observation("run-auto-route", "root", StopObservation::new("root done"))
            .with_stop_observation(
                "run-auto-route",
                "finish",
                StopObservation::new("finish done"),
            ),
    );

    assert_eq!(
        report
            .stop_decisions
            .iter()
            .map(|decision| decision.kind)
            .collect::<Vec<_>>(),
        vec![StopDecisionKind::Allow, StopDecisionKind::Allow]
    );
    assert_eq!(
        driver.runtime().state().run_status("run-auto-route"),
        Some(RunStatus::Completed)
    );
    assert_eq!(
        driver
            .runtime()
            .state()
            .activations
            .get(&activation_key("run-auto-route", "finish"))
            .map(|activation| activation.status),
        Some(ActivationStatus::Completed)
    );
}

#[test]
fn driver_route_activations_keep_locked_target_contracts() {
    let lock = route_lock_with_finish_contract();
    let lock_id = lock.id().to_string();
    let mut driver = DriverState::default();
    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::StartRun {
            run_id: "run-route-contract".into(),
            nodes: vec![NodeSpec::new("root")],
        }),
    );
    driver
        .runtime_mut()
        .apply_flow_lock(
            "run-route-contract",
            FlowLockMode::FutureActivations,
            &lock_id,
            "hash-route",
        )
        .unwrap();
    driver
        .runtime_mut()
        .deliver_artifact("run-route-contract", "root", "ready", "done")
        .unwrap();

    driver.tick(
        DriverTickInput::default()
            .with_route_lock(lock)
            .with_stop_observation(
                "run-route-contract",
                "root",
                StopObservation::new("root done"),
            ),
    );

    let finish = driver
        .runtime()
        .state()
        .activations
        .get(&activation_key("run-route-contract", "finish"))
        .expect("route target should be activated");
    assert_eq!(
        finish.stop_contract.required_artifacts(),
        &["summary".to_string()]
    );

    let blocked = driver.tick(DriverTickInput::default().with_stop_observation(
        "run-route-contract",
        "finish",
        StopObservation::new("finish done"),
    ));

    assert_eq!(blocked.stop_decisions[0].kind, StopDecisionKind::Deny);
    assert_eq!(
        blocked.stop_decisions[0].missing_artifacts,
        vec!["summary".to_string()]
    );
}

#[test]
fn driver_tick_respects_tick_limit_when_route_work_remains() {
    let lock = route_preview_lock(vec![FlowRoute {
        predicate: "exists(artifact.ready)".into(),
        for_each: None,
        activate: "finish".into(),
    }]);
    let lock_id = lock.id().to_string();
    let mut driver = DriverState::default();
    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::StartRun {
            run_id: "run-budgeted-route".into(),
            nodes: vec![NodeSpec::new("root")],
        }),
    );
    driver
        .runtime_mut()
        .apply_flow_lock(
            "run-budgeted-route",
            FlowLockMode::FutureActivations,
            &lock_id,
            "hash-route",
        )
        .unwrap();
    driver
        .runtime_mut()
        .deliver_artifact("run-budgeted-route", "root", "ready", "done")
        .unwrap();

    let report = driver.tick(
        DriverTickInput::default()
            .with_loop_budget(LoopBudget {
                tick_limit: 1,
                action_limit: 16,
            })
            .with_route_lock(lock)
            .with_stop_observation(
                "run-budgeted-route",
                "root",
                StopObservation::new("root done"),
            )
            .with_stop_observation(
                "run-budgeted-route",
                "finish",
                StopObservation::new("finish done"),
            ),
    );

    assert_eq!(report.stop_decisions.len(), 1);
    assert_eq!(
        driver.runtime().state().run_status("run-budgeted-route"),
        Some(RunStatus::Running)
    );
    assert_eq!(
        driver
            .runtime()
            .state()
            .activations
            .get(&activation_key("run-budgeted-route", "finish"))
            .map(|activation| activation.status),
        Some(ActivationStatus::Running)
    );
}

#[test]
fn driver_tick_zero_tick_limit_skips_automatic_driver_loop() {
    let mut driver = DriverState::default();

    let report = driver.tick(
        DriverTickInput::default()
            .with_loop_budget(LoopBudget {
                tick_limit: 0,
                action_limit: 16,
            })
            .with_control(ControlCommand::StartRun {
                run_id: "run-zero-ticks".into(),
                nodes: vec![NodeSpec::new("root")],
            })
            .with_stop_observation("run-zero-ticks", "root", StopObservation::new("root done")),
    );

    assert_eq!(report.stop_decisions, Vec::new());
    assert_eq!(
        driver.runtime().state().run_status("run-zero-ticks"),
        Some(RunStatus::Running)
    );
    assert_eq!(
        driver
            .runtime()
            .state()
            .activations
            .get(&activation_key("run-zero-ticks", "root"))
            .map(|activation| activation.status),
        Some(ActivationStatus::Pending)
    );
}

#[test]
fn stop_validation_zero_per_tick_defers_without_recording_attempt() {
    let mut driver = DriverState::default();
    driver
        .runtime_mut()
        .start_run(
            "run-stop-budget",
            vec![node_with_contract("review", &["summary"], &[])],
        )
        .unwrap();

    let deferred = driver.tick(
        DriverTickInput::default()
            .with_budget(TickBudget {
                stop_validation_attempt_limit: 2,
                stop_validations_per_tick: 0,
            })
            .with_stop_observation(
                "run-stop-budget",
                "review",
                StopObservation::new("pane exited"),
            ),
    );

    assert_eq!(deferred.stop_decisions[0].kind, StopDecisionKind::Yield);
    assert_eq!(
        driver
            .runtime()
            .state()
            .stop_validation_attempts
            .get(&activation_key("run-stop-budget", "review")),
        None
    );

    let validated = driver.tick(
        DriverTickInput::default()
            .with_budget(TickBudget {
                stop_validation_attempt_limit: 2,
                ..TickBudget::default()
            })
            .with_stop_observation(
                "run-stop-budget",
                "review",
                StopObservation::new("pane exited"),
            ),
    );

    assert_eq!(validated.stop_decisions[0].kind, StopDecisionKind::Deny);
    assert_eq!(validated.stop_decisions[0].attempt, 1);
}

#[test]
fn driver_tick_zero_action_limit_prevents_route_actuate_and_complete_work() {
    let lock = route_preview_lock(vec![FlowRoute {
        predicate: "exists(artifact.ready)".into(),
        for_each: None,
        activate: "finish".into(),
    }]);
    let lock_id = lock.id().to_string();
    let mut driver = DriverState::default();
    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::StartRun {
            run_id: "run-zero-actions".into(),
            nodes: vec![NodeSpec::new("root")],
        }),
    );
    driver
        .runtime_mut()
        .apply_flow_lock(
            "run-zero-actions",
            FlowLockMode::FutureActivations,
            &lock_id,
            "hash-route",
        )
        .unwrap();
    driver
        .runtime_mut()
        .deliver_artifact("run-zero-actions", "root", "ready", "done")
        .unwrap();

    let report = driver.tick(
        DriverTickInput::default()
            .with_loop_budget(LoopBudget {
                tick_limit: 8,
                action_limit: 0,
            })
            .with_route_lock(lock)
            .with_stop_observation(
                "run-zero-actions",
                "root",
                StopObservation::new("root done"),
            ),
    );

    assert_eq!(report.stop_decisions.len(), 1);
    assert_eq!(report.stop_decisions[0].kind, StopDecisionKind::Allow);
    assert_eq!(
        driver.runtime().state().run_status("run-zero-actions"),
        Some(RunStatus::Running)
    );
    assert!(
        !driver
            .runtime()
            .state()
            .activations
            .contains_key(&activation_key("run-zero-actions", "finish"))
    );
}

#[test]
fn stop_run_transitions_to_stopped_and_cancels_active_activations() {
    let mut driver = DriverState::default();
    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::StartRun {
            run_id: "run-stop-control".into(),
            nodes: vec![NodeSpec::new("root")],
        }),
    );

    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::StopRun {
            run_id: "run-stop-control".into(),
        }),
    );

    assert_eq!(
        driver.runtime().state().run_status("run-stop-control"),
        Some(RunStatus::Stopped)
    );
    assert_eq!(
        driver
            .runtime()
            .state()
            .activations
            .get(&activation_key("run-stop-control", "root"))
            .map(|activation| activation.status),
        Some(ActivationStatus::Cancelled)
    );
    assert_eq!(
        driver
            .runtime()
            .events()
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::RunStatusChanged { run_id, status }
                    if run_id == "run-stop-control" =>
                {
                    Some(*status)
                }
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![RunStatus::Running, RunStatus::Stopping, RunStatus::Stopped]
    );
}

#[test]
fn stop_observation_ignores_terminal_activations() {
    let mut driver = DriverState::default();
    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::StartRun {
            run_id: "run-late-stop".into(),
            nodes: vec![NodeSpec::new("root")],
        }),
    );
    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::StopRun {
            run_id: "run-late-stop".into(),
        }),
    );

    let late = driver.tick(DriverTickInput::default().with_stop_observation(
        "run-late-stop",
        "root",
        StopObservation::new("late pane exit"),
    ));

    assert_eq!(late.stop_decisions, Vec::new());
    assert_eq!(
        driver
            .runtime()
            .state()
            .activations
            .get(&activation_key("run-late-stop", "root"))
            .map(|activation| activation.status),
        Some(ActivationStatus::Cancelled)
    );
    assert_eq!(
        driver
            .runtime()
            .state()
            .stop_validation_attempts
            .get(&activation_key("run-late-stop", "root")),
        None
    );
}

#[test]
fn stop_run_is_not_overridden_by_continuous_quiescence_completion() {
    let mut driver = DriverState::default();
    driver.tick(
        DriverTickInput::default()
            .with_completion_mode(RunCompletionMode::Continuous)
            .with_control(ControlCommand::StartRun {
                run_id: "run-stop-quiescent".into(),
                nodes: vec![NodeSpec::new("root")],
            })
            .with_stop_observation(
                "run-stop-quiescent",
                "root",
                StopObservation::new("root done"),
            ),
    );
    assert_eq!(
        driver.runtime().state().run_status("run-stop-quiescent"),
        Some(RunStatus::Quiescent)
    );

    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::StopRun {
            run_id: "run-stop-quiescent".into(),
        }),
    );

    assert_eq!(
        driver.runtime().state().run_status("run-stop-quiescent"),
        Some(RunStatus::Stopped)
    );
}

#[test]
fn manual_completion_requires_control_after_quiescence() {
    let mut driver = DriverState::default();

    driver.tick(
        DriverTickInput::default()
            .with_completion_mode(RunCompletionMode::Manual)
            .with_control(ControlCommand::StartRun {
                run_id: "run-manual".into(),
                nodes: vec![NodeSpec::new("root")],
            })
            .with_stop_observation("run-manual", "root", StopObservation::new("pane exited")),
    );

    assert_eq!(
        driver.runtime().state().run_status("run-manual"),
        Some(RunStatus::Quiescent)
    );

    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::CompleteRun {
            run_id: "run-manual".into(),
        }),
    );

    assert_eq!(
        driver.runtime().state().run_status("run-manual"),
        Some(RunStatus::Completed)
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
fn preview_flow_routes_returns_plan_without_appending_runtime_events() {
    let lock = flow_lock(
        &FlowDraft {
            nodes: vec![
                FlowNode {
                    id: "root".into(),
                    ..FlowNode::default()
                },
                FlowNode {
                    id: "finish".into(),
                    ..FlowNode::default()
                },
            ],
            resources: vec![FlowResource {
                id: "readme.main".into(),
                kind: ResourceKind::Readme,
                source: "inline:Preview local routes.".into(),
            }],
            routes: vec![FlowRoute {
                predicate: "exists(artifact.ready)".into(),
                for_each: None,
                activate: "finish".into(),
            }],
            ..FlowDraft::default()
        },
        FlowCheckMode::Core,
    )
    .unwrap();
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-a", vec![NodeSpec::new("root")])
        .unwrap();
    runtime
        .deliver_artifact("run-a", "root", "ready", "done")
        .unwrap();
    let event_count = runtime.events().len();

    let routes = preview_flow_routes(runtime.state(), "run-a", &lock).unwrap();

    assert_eq!(runtime.events().len(), event_count);
    assert!(routes[0].matched);
    assert_eq!(
        routes[0].planned_activations[0].activation_id,
        "finish".to_string()
    );
    assert!(
        !runtime
            .state()
            .activations
            .contains_key(&activation_key("run-a", "finish"))
    );
}

#[test]
fn preview_flow_routes_matches_artifact_and_board_keys_containing_event() {
    let lock = route_preview_lock(vec![
        FlowRoute {
            predicate: "artifact.event.status".into(),
            for_each: None,
            activate: "artifact_target".into(),
        },
        FlowRoute {
            predicate: "board.event.ready".into(),
            for_each: None,
            activate: "board_target".into(),
        },
    ]);
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-event-keys", vec![NodeSpec::new("root")])
        .unwrap();
    runtime
        .deliver_artifact("run-event-keys", "root", "event.status", "ready")
        .unwrap();
    runtime
        .patch_board(
            "run-event-keys",
            "root",
            BoardPatch::new("event.ready", "true"),
        )
        .unwrap();

    let routes = preview_flow_routes(runtime.state(), "run-event-keys", &lock).unwrap();

    assert_eq!(routes.len(), 2);
    assert!(routes[0].matched);
    assert_eq!(routes[0].reason, None);
    assert_eq!(
        routes[0].planned_activations[0].activation_id,
        "artifact_target"
    );
    assert!(routes[1].matched);
    assert_eq!(routes[1].reason, None);
    assert_eq!(
        routes[1].planned_activations[0].activation_id,
        "board_target"
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
        Some("flow-lock-application:9")
    );
    let first_application = runtime
        .state()
        .flow_lock_applications
        .get("flow-lock-application:5")
        .expect("first flow lock application should remain addressable");
    let second_application = runtime
        .state()
        .flow_lock_applications
        .get("flow-lock-application:9")
        .expect("second flow lock application should be addressable");

    assert_eq!(first_application.lock_id, "lock-a");
    assert_eq!(first_application.content_hash, "sha256:first");
    assert_eq!(first_application.event_sequence, 5);
    assert_eq!(second_application.lock_id, "lock-b");
    assert_eq!(second_application.content_hash, "sha256:second");
    assert!(matches!(
        runtime
            .events()
            .iter()
            .find(|event| event.sequence == 9)
            .map(|event| &event.payload),
        Some(EventPayload::FlowUpdate {
            run_id,
            status,
            mode,
            lock_id,
            contract_hash,
        }) if run_id == "run-a"
            && *status == FlowUpdateStatus::Applied
            && *mode == FlowLockMode::CheckpointRestart
            && lock_id == "lock-b"
            && contract_hash == "sha256:second"
    ));
}
