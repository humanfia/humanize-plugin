use std::collections::BTreeSet;

use humanize_plugin::flow::{
    ArtifactRef, FlowCheckMode, FlowDraft, FlowNode, FlowPredicate, FlowResource, FlowRoute,
    ResourceKind, flow_check, flow_lock,
};
use humanize_plugin::runtime::{
    ActivationStatus, BoardPatch, ControlCommand, DriverState, DriverTickInput, Event,
    EventPayload, FlowLockMode, LoopBudget, NodeSpec, RunMode, RunStatus, Runtime, RuntimeState,
    StopObservation, preview_flow_routes,
};
use serde_json::json;

fn activation_key(run_id: &str, activation_id: &str) -> (String, String) {
    (run_id.to_owned(), activation_id.to_owned())
}

fn route_lock(routes: Vec<FlowRoute>) -> humanize_plugin::flow::FlowLock {
    let mut nodes = vec!["root".to_string()];
    for route in &routes {
        if !nodes.contains(&route.activate) {
            nodes.push(route.activate.clone());
        }
    }
    flow_lock(
        &FlowDraft {
            nodes: nodes
                .into_iter()
                .map(|id| FlowNode {
                    id,
                    ..FlowNode::default()
                })
                .collect(),
            routes,
            resources: vec![FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: "inline:Generation-aware runtime test flow.".into(),
            }],
            ..FlowDraft::default()
        },
        FlowCheckMode::Core,
    )
    .unwrap()
}

fn start_driver(run_id: &str, mode: RunMode, activation_limit: u64) -> DriverState {
    let mut driver = DriverState::default();
    let report = driver.tick(
        DriverTickInput::default()
            .with_run_mode(mode)
            .with_activation_limit(activation_limit)
            .with_control(ControlCommand::StartRun {
                run_id: run_id.into(),
                nodes: vec![NodeSpec::new("root")],
            }),
    );
    assert!(report.control_errors.is_empty());
    driver
}

fn complete_activation(driver: &mut DriverState, run_id: &str, activation_id: &str) {
    let report = driver.tick(DriverTickInput::default().with_stop_observation(
        run_id,
        activation_id,
        StopObservation::new("complete"),
    ));
    assert_eq!(report.stop_decisions.len(), 1);
}

#[test]
fn run_started_persists_mode_limit_and_derives_used_from_activations() {
    let mut runtime = Runtime::default();
    runtime
        .start_run_with_options(
            "run-config",
            vec![NodeSpec::new("root")],
            RunMode::Continuous,
            4,
        )
        .unwrap();
    runtime
        .set_run_status("run-config", RunStatus::Running)
        .unwrap();
    runtime
        .activate_node("run-config", &NodeSpec::new("later"), None)
        .unwrap();

    assert!(matches!(
        &runtime.events()[0].payload,
        EventPayload::RunStarted {
            run_id,
            mode: RunMode::Continuous,
            activation_limit: 4,
            stop_attempt_limit: 3,
        } if run_id == "run-config"
    ));
    assert_eq!(
        runtime.state().run_mode("run-config"),
        Some(RunMode::Continuous)
    );
    assert_eq!(
        runtime.state().initial_activation_limit("run-config"),
        Some(4)
    );
    assert_eq!(runtime.state().activation_limit("run-config"), Some(4));
    assert_eq!(runtime.state().activations_used("run-config"), 2);

    let replayed = RuntimeState::from_events(runtime.events());
    assert_eq!(replayed, *runtime.state());
    assert_eq!(replayed.activations_used("run-config"), 2);
}

#[test]
fn artifact_and_board_fact_versions_are_runtime_event_sequences_per_key() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-facts", vec![NodeSpec::new("root")])
        .unwrap();
    runtime
        .deliver_artifact("run-facts", "root", "ready", "first")
        .unwrap();
    let artifact_version = runtime.events().last().unwrap().sequence;
    let first_a = runtime
        .patch_board(
            "run-facts",
            "root",
            BoardPatch::new("a", "one").unwrap().expect_version(0),
        )
        .unwrap();
    let first_b = runtime
        .patch_board(
            "run-facts",
            "root",
            BoardPatch::new("b", "two").unwrap().expect_version(0),
        )
        .unwrap();
    let second_a = runtime
        .patch_board(
            "run-facts",
            "root",
            BoardPatch::new("a", "three")
                .unwrap()
                .expect_version(first_a),
        )
        .unwrap();

    assert_eq!(
        runtime.state().artifact_fact_version("run-facts", "ready"),
        Some(artifact_version)
    );
    assert_eq!(
        runtime.state().board_fact_version("run-facts", "a"),
        Some(second_a)
    );
    assert_eq!(
        runtime.state().board_fact_version("run-facts", "b"),
        Some(first_b)
    );
    assert_eq!(first_a, runtime.events()[3].sequence);
    assert_eq!(first_b, runtime.events()[4].sequence);
    assert_eq!(second_a, runtime.events()[5].sequence);
    assert_eq!(
        RuntimeState::from_events(runtime.events()),
        *runtime.state()
    );
}

#[test]
fn canonical_route_identity_survives_reorder_and_duplicate_routes_are_rejected() {
    let first = FlowRoute {
        predicate: FlowPredicate::exists_artifact("ready").unwrap(),
        for_each: None,
        activate: "finish".into(),
    };
    let second = FlowRoute {
        predicate: FlowPredicate::exists_board("approved").unwrap(),
        for_each: None,
        activate: "publish".into(),
    };
    let lock_a = route_lock(vec![first.clone(), second.clone()]);
    let lock_b = route_lock(vec![second.clone(), first.clone()]);
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-route-id", vec![NodeSpec::new("root")])
        .unwrap();
    runtime
        .deliver_artifact("run-route-id", "root", "ready", "yes")
        .unwrap();
    runtime
        .patch_board(
            "run-route-id",
            "root",
            BoardPatch::new("approved", "true").unwrap(),
        )
        .unwrap();

    let ids_a = preview_flow_routes(runtime.state(), "run-route-id", &lock_a)
        .unwrap()
        .into_iter()
        .map(|route| route.route_id)
        .collect::<BTreeSet<_>>();
    let ids_b = preview_flow_routes(runtime.state(), "run-route-id", &lock_b)
        .unwrap()
        .into_iter()
        .map(|route| route.route_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(ids_a, ids_b);

    let duplicate = FlowDraft {
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
        routes: vec![first.clone(), first],
        resources: vec![FlowResource {
            id: "README.md".into(),
            kind: ResourceKind::Readme,
            source: "inline:Duplicate route check.".into(),
        }],
        ..FlowDraft::default()
    };
    assert!(
        flow_check(&duplicate, FlowCheckMode::Core)
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "FLOW_DUPLICATE_ROUTE")
    );
}

#[test]
fn one_trigger_applies_once_and_new_fact_version_reactivates_the_lane() {
    let lock = route_lock(vec![FlowRoute {
        predicate: FlowPredicate::exists_artifact("ready").unwrap(),
        for_each: None,
        activate: "finish".into(),
    }]);
    let mut driver = start_driver("run-trigger", RunMode::Continuous, 8);
    driver
        .runtime_mut()
        .apply_flow_lock(
            "run-trigger",
            FlowLockMode::FutureActivations,
            lock.id(),
            "hash",
        )
        .unwrap();
    driver
        .runtime_mut()
        .deliver_artifact("run-trigger", "root", "ready", "one")
        .unwrap();
    let first_version = driver.runtime().events().last().unwrap().sequence;

    let first = driver.tick(DriverTickInput::default().with_route_lock(lock.clone()));
    assert_eq!(first.route_decisions.len(), 1);
    assert_eq!(first.route_decisions[0].applied_activation_ids, ["finish"]);
    assert_eq!(
        driver
            .runtime()
            .state()
            .activations
            .get(&activation_key("run-trigger", "finish"))
            .unwrap()
            .activation_generation,
        0
    );
    assert_eq!(
        driver
            .tick(DriverTickInput::default().with_route_lock(lock.clone()))
            .route_decisions,
        []
    );

    driver
        .runtime_mut()
        .deliver_artifact("run-trigger", "root", "ready", "two")
        .unwrap();
    let second_version = driver.runtime().events().last().unwrap().sequence;
    let second = driver.tick(DriverTickInput::default().with_route_lock(lock));
    assert_eq!(second.route_decisions.len(), 1);
    let second_id = &second.route_decisions[0].applied_activation_ids[0];
    assert_ne!(second_id, "finish");
    let activation = driver
        .runtime()
        .state()
        .activations
        .get(&activation_key("run-trigger", second_id))
        .unwrap();
    assert_eq!(activation.activation_generation, 1);
    assert_eq!(
        activation.trigger.as_ref().unwrap().fact_version,
        second_version
    );
    assert_ne!(first_version, second_version);
}

#[test]
fn for_each_artifact_version_is_trigger_and_predicate_fact_is_only_a_gate() {
    let lock = route_lock(vec![FlowRoute {
        predicate: FlowPredicate::truthy_board("open").unwrap(),
        for_each: Some(ArtifactRef::new("items").unwrap()),
        activate: "process".into(),
    }]);
    let mut driver = start_driver("run-fanout-trigger", RunMode::Continuous, 12);
    driver
        .runtime_mut()
        .apply_flow_lock(
            "run-fanout-trigger",
            FlowLockMode::FutureActivations,
            lock.id(),
            "hash",
        )
        .unwrap();
    driver
        .runtime_mut()
        .patch_board(
            "run-fanout-trigger",
            "root",
            BoardPatch::new("open", "true").unwrap(),
        )
        .unwrap();
    driver
        .runtime_mut()
        .deliver_artifact("run-fanout-trigger", "root", "items", "a\nb")
        .unwrap();
    let first_items_version = driver.runtime().events().last().unwrap().sequence;
    let first = driver.tick(DriverTickInput::default().with_route_lock(lock.clone()));
    assert_eq!(first.route_decisions[0].applied_activation_ids.len(), 2);

    let open_version = driver
        .runtime()
        .state()
        .board_fact_version("run-fanout-trigger", "open")
        .unwrap();
    driver
        .runtime_mut()
        .patch_board(
            "run-fanout-trigger",
            "root",
            BoardPatch::new("open", "still true")
                .unwrap()
                .expect_version(open_version),
        )
        .unwrap();
    assert_eq!(
        driver
            .tick(DriverTickInput::default().with_route_lock(lock.clone()))
            .route_decisions,
        []
    );

    driver
        .runtime_mut()
        .deliver_artifact("run-fanout-trigger", "root", "items", "a\nb")
        .unwrap();
    let second_items_version = driver.runtime().events().last().unwrap().sequence;
    let second = driver.tick(DriverTickInput::default().with_route_lock(lock));
    assert_eq!(second.route_decisions[0].applied_activation_ids.len(), 2);
    for activation_id in &second.route_decisions[0].applied_activation_ids {
        let activation = driver
            .runtime()
            .state()
            .activations
            .get(&activation_key("run-fanout-trigger", activation_id))
            .unwrap();
        let trigger = activation.trigger.as_ref().unwrap();
        assert_eq!(trigger.fact_ref, "artifact.items");
        assert_eq!(trigger.fact_version, second_items_version);
        assert_eq!(activation.activation_generation, 1);
    }
    assert_ne!(first_items_version, second_items_version);
}

#[test]
fn fanout_budget_exhaustion_is_atomic_and_resume_only_raises_absolute_limit() {
    let lock = route_lock(vec![FlowRoute {
        predicate: FlowPredicate::exists_artifact("items").unwrap(),
        for_each: Some(ArtifactRef::new("items").unwrap()),
        activate: "process".into(),
    }]);
    let mut driver = start_driver("run-budget", RunMode::Continuous, 2);
    driver
        .runtime_mut()
        .apply_flow_lock(
            "run-budget",
            FlowLockMode::FutureActivations,
            lock.id(),
            "hash",
        )
        .unwrap();
    driver
        .runtime_mut()
        .deliver_artifact("run-budget", "root", "items", "a\nb")
        .unwrap();
    let before = driver.runtime().events().len();

    let exhausted = driver.tick(DriverTickInput::default().with_route_lock(lock.clone()));
    assert!(exhausted.route_decisions.is_empty());
    assert_eq!(driver.runtime().state().activations_used("run-budget"), 1);
    assert_eq!(
        driver.runtime().state().run_status("run-budget"),
        Some(RunStatus::Paused)
    );
    assert_eq!(
        driver.runtime().state().run_status_reason("run-budget"),
        Some("activation_limit_exhausted")
    );
    assert!(
        driver.runtime().events()[before..]
            .iter()
            .all(|event| !matches!(event.payload, EventPayload::NodeActivated { .. }))
    );

    let stale_resume = driver.tick(
        DriverTickInput::default()
            .with_activation_limit(2)
            .with_control(ControlCommand::ResumeRun {
                run_id: "run-budget".into(),
            })
            .with_route_lock(lock.clone()),
    );
    assert_eq!(stale_resume.control_errors.len(), 1);
    assert_eq!(driver.runtime().state().activations_used("run-budget"), 1);

    let resumed = driver.tick(
        DriverTickInput::default()
            .with_activation_limit(3)
            .with_control(ControlCommand::ResumeRun {
                run_id: "run-budget".into(),
            })
            .with_route_lock(lock),
    );
    assert!(resumed.control_errors.is_empty());
    assert_eq!(resumed.route_decisions[0].applied_activation_ids.len(), 2);
    assert_eq!(driver.runtime().state().activations_used("run-budget"), 3);
    assert_eq!(
        driver.runtime().state().activation_limit("run-budget"),
        Some(3)
    );
}

#[test]
fn action_limit_counts_a_fanout_route_firing_once_not_each_activation() {
    let lock = route_lock(vec![FlowRoute {
        predicate: FlowPredicate::exists_artifact("items").unwrap(),
        for_each: Some(ArtifactRef::new("items").unwrap()),
        activate: "process".into(),
    }]);
    let mut driver = start_driver("run-wide-fanout", RunMode::Continuous, 64);
    driver
        .runtime_mut()
        .apply_flow_lock(
            "run-wide-fanout",
            FlowLockMode::FutureActivations,
            lock.id(),
            "hash",
        )
        .unwrap();
    let items = (0..40)
        .map(|index| format!("item-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    driver
        .runtime_mut()
        .deliver_artifact("run-wide-fanout", "root", "items", items)
        .unwrap();
    let before = driver.runtime().events().len();

    let report = driver.tick(
        DriverTickInput::default()
            .with_loop_budget(LoopBudget {
                tick_limit: 1,
                action_limit: 1,
            })
            .with_route_lock(lock),
    );

    assert_eq!(report.route_decisions.len(), 1);
    assert_eq!(report.route_decisions[0].applied_activation_ids.len(), 40);
    assert_eq!(
        driver.runtime().state().activations_used("run-wide-fanout"),
        41
    );
    assert_eq!(
        driver.runtime().events()[before..]
            .iter()
            .filter(|event| matches!(event.payload, EventPayload::NodeActivated { .. }))
            .count(),
        40
    );
}

#[test]
fn explicit_activation_fanout_and_route_share_one_generation_rule() {
    let mut runtime = Runtime::default();
    runtime
        .start_run("run-public-generation", vec![NodeSpec::new("root")])
        .unwrap();
    runtime
        .set_run_status("run-public-generation", RunStatus::Running)
        .unwrap();
    let lane = NodeSpec::new("lane");

    let first = runtime
        .activate_node("run-public-generation", &lane, None)
        .unwrap();
    let second = runtime
        .activate_node("run-public-generation", &lane, None)
        .unwrap();
    assert_eq!(first, "lane");
    assert_eq!(second, "lane~g1");

    let lock = route_lock(vec![FlowRoute {
        predicate: FlowPredicate::exists_artifact("ready").unwrap(),
        for_each: None,
        activate: "lane".into(),
    }]);
    runtime
        .apply_flow_lock(
            "run-public-generation",
            FlowLockMode::FutureActivations,
            lock.id(),
            "hash",
        )
        .unwrap();
    runtime
        .deliver_artifact("run-public-generation", "root", "ready", "yes")
        .unwrap();
    let preview = preview_flow_routes(runtime.state(), "run-public-generation", &lock).unwrap();
    assert_eq!(preview[0].planned_activations[0].activation_id, "lane~g2");

    runtime
        .deliver_artifact("run-public-generation", "root", "items", "a\nb")
        .unwrap();
    let process = NodeSpec::new("process").with_for_each(ArtifactRef::new("items").unwrap());
    let first_batch = runtime
        .fanout_from_artifact("run-public-generation", &process, "items")
        .unwrap();
    let second_batch = runtime
        .fanout_from_artifact("run-public-generation", &process, "items")
        .unwrap();
    assert_eq!(
        first_batch,
        vec!["process:items/0".to_string(), "process:items/1".to_string()]
    );
    assert_eq!(
        second_batch,
        vec![
            "process:items/0~g1".to_string(),
            "process:items/1~g1".to_string()
        ]
    );
}

#[test]
fn finite_continuous_and_manual_modes_have_distinct_quiescence_behavior() {
    let lock = route_lock(vec![FlowRoute {
        predicate: FlowPredicate::exists_artifact("ready").unwrap(),
        for_each: None,
        activate: "finish".into(),
    }]);

    let mut finite = start_driver("run-finite", RunMode::Finite, 4);
    complete_activation(&mut finite, "run-finite", "root");
    assert_eq!(
        finite.runtime().state().run_status("run-finite"),
        Some(RunStatus::Completed)
    );

    let mut continuous = start_driver("run-continuous", RunMode::Continuous, 4);
    continuous
        .runtime_mut()
        .apply_flow_lock(
            "run-continuous",
            FlowLockMode::FutureActivations,
            lock.id(),
            "hash",
        )
        .unwrap();
    complete_activation(&mut continuous, "run-continuous", "root");
    assert_eq!(
        continuous.runtime().state().run_status("run-continuous"),
        Some(RunStatus::Quiescent)
    );
    continuous
        .runtime_mut()
        .deliver_artifact("run-continuous", "root", "ready", "yes")
        .unwrap();
    let woke = continuous.tick(DriverTickInput::default().with_route_lock(lock.clone()));
    assert_eq!(woke.route_decisions.len(), 1);
    assert_eq!(
        continuous.runtime().state().run_status("run-continuous"),
        Some(RunStatus::Running)
    );

    let mut manual = start_driver("run-manual-mode", RunMode::Manual, 4);
    manual
        .runtime_mut()
        .apply_flow_lock(
            "run-manual-mode",
            FlowLockMode::FutureActivations,
            lock.id(),
            "hash",
        )
        .unwrap();
    complete_activation(&mut manual, "run-manual-mode", "root");
    manual
        .runtime_mut()
        .deliver_artifact("run-manual-mode", "root", "ready", "yes")
        .unwrap();
    assert!(
        manual
            .tick(DriverTickInput::default().with_route_lock(lock.clone()))
            .route_decisions
            .is_empty()
    );
    let resumed = manual.tick(
        DriverTickInput::default()
            .with_control(ControlCommand::ResumeRun {
                run_id: "run-manual-mode".into(),
            })
            .with_route_lock(lock),
    );
    assert_eq!(resumed.route_decisions.len(), 1);
}

#[test]
fn empty_runs_settle_by_mode_when_no_trigger_is_pending() {
    for (mode, expected) in [
        (RunMode::Finite, RunStatus::Completed),
        (RunMode::Continuous, RunStatus::Quiescent),
        (RunMode::Manual, RunStatus::Quiescent),
    ] {
        let run_id = format!("empty-{mode:?}").to_ascii_lowercase();
        let mut driver = DriverState::default();
        driver.tick(
            DriverTickInput::default()
                .with_run_mode(mode)
                .with_activation_limit(0)
                .with_control(ControlCommand::StartRun {
                    run_id: run_id.clone(),
                    nodes: Vec::new(),
                }),
        );
        assert_eq!(driver.runtime().state().run_status(&run_id), Some(expected));
    }
}

#[test]
fn pause_stops_scheduling_but_allows_delivery_and_stop_without_overwriting_pause() {
    let lock = route_lock(vec![FlowRoute {
        predicate: FlowPredicate::exists_artifact("ready").unwrap(),
        for_each: None,
        activate: "finish".into(),
    }]);
    let mut driver = start_driver("run-paused", RunMode::Continuous, 4);
    driver
        .runtime_mut()
        .apply_flow_lock(
            "run-paused",
            FlowLockMode::FutureActivations,
            lock.id(),
            "hash",
        )
        .unwrap();
    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::PauseRun {
            run_id: "run-paused".into(),
        }),
    );
    driver
        .runtime_mut()
        .deliver_artifact("run-paused", "root", "ready", "yes")
        .unwrap();
    let paused = driver.tick(
        DriverTickInput::default()
            .with_route_lock(lock.clone())
            .with_stop_observation("run-paused", "root", StopObservation::new("done")),
    );
    assert!(paused.route_decisions.is_empty());
    assert_eq!(paused.stop_decisions.len(), 1);
    assert_eq!(
        driver.runtime().state().run_status("run-paused"),
        Some(RunStatus::Paused)
    );
    assert!(
        !driver
            .runtime()
            .state()
            .activations
            .contains_key(&activation_key("run-paused", "finish"))
    );

    let resumed = driver.tick(
        DriverTickInput::default()
            .with_control(ControlCommand::ResumeRun {
                run_id: "run-paused".into(),
            })
            .with_route_lock(lock),
    );
    assert_eq!(resumed.route_decisions.len(), 1);
}

#[test]
fn paused_run_rejects_explicit_activation_without_event_or_state_change() {
    let mut driver = start_driver("run-paused-explicit", RunMode::Continuous, 4);
    driver.tick(
        DriverTickInput::default().with_control(ControlCommand::PauseRun {
            run_id: "run-paused-explicit".into(),
        }),
    );
    let before_events = driver.runtime().events().to_vec();
    let before_state = driver.runtime().state().clone();

    let error = driver
        .runtime_mut()
        .activate_node("run-paused-explicit", &NodeSpec::new("later"), None)
        .unwrap_err();

    assert_eq!(error.to_string(), "run run-paused-explicit is paused");
    assert_eq!(driver.runtime().events(), before_events);
    assert_eq!(driver.runtime().state(), &before_state);
}

#[test]
fn explicit_activation_and_fanout_require_running_without_mutation() {
    for status in [
        RunStatus::Paused,
        RunStatus::Quiescent,
        RunStatus::Completed,
        RunStatus::Stopped,
        RunStatus::Failed,
    ] {
        let run_id = format!("run-explicit-{status:?}").to_ascii_lowercase();
        let mut driver = start_driver(&run_id, RunMode::Continuous, 8);
        driver
            .runtime_mut()
            .deliver_artifact(&run_id, "root", "items", "alpha\nbeta")
            .unwrap();
        driver
            .runtime_mut()
            .set_run_status(&run_id, status)
            .unwrap();
        let before_events = driver.runtime().events().to_vec();
        let before_state = driver.runtime().state().clone();

        let activation =
            driver
                .runtime_mut()
                .activate_node(&run_id, &NodeSpec::new("manual"), None);
        assert!(activation.is_err(), "status={status:?}");
        assert_eq!(
            driver.runtime().events(),
            before_events,
            "status={status:?}"
        );
        assert_eq!(driver.runtime().state(), &before_state, "status={status:?}");

        let fanout = driver.runtime_mut().fanout_from_artifact(
            &run_id,
            &NodeSpec::new("batch").with_for_each(ArtifactRef::new("items").unwrap()),
            "items",
        );
        assert!(fanout.is_err(), "status={status:?}");
        assert_eq!(
            driver.runtime().events(),
            before_events,
            "status={status:?}"
        );
        assert_eq!(driver.runtime().state(), &before_state, "status={status:?}");
    }
}

#[test]
fn replayed_trigger_prevents_duplicate_activation_and_preserves_mode_and_budget() {
    let lock = route_lock(vec![FlowRoute {
        predicate: FlowPredicate::exists_artifact("ready").unwrap(),
        for_each: None,
        activate: "finish".into(),
    }]);
    let mut driver = start_driver("run-replay-trigger", RunMode::Continuous, 5);
    driver
        .runtime_mut()
        .apply_flow_lock(
            "run-replay-trigger",
            FlowLockMode::FutureActivations,
            lock.id(),
            "hash",
        )
        .unwrap();
    driver
        .runtime_mut()
        .deliver_artifact("run-replay-trigger", "root", "ready", "one")
        .unwrap();
    driver.tick(DriverTickInput::default().with_route_lock(lock.clone()));
    let events = driver.runtime().events().to_vec();

    let mut restarted = DriverState::from_runtime(Runtime::from_events(events));
    let replay = restarted.tick(DriverTickInput::default().with_route_lock(lock.clone()));
    assert!(replay.route_decisions.is_empty());
    assert_eq!(
        restarted.runtime().state().run_mode("run-replay-trigger"),
        Some(RunMode::Continuous)
    );
    assert_eq!(
        restarted
            .runtime()
            .state()
            .activation_limit("run-replay-trigger"),
        Some(5)
    );

    restarted
        .runtime_mut()
        .deliver_artifact("run-replay-trigger", "root", "ready", "two")
        .unwrap();
    let next = restarted.tick(DriverTickInput::default().with_route_lock(lock));
    assert_eq!(next.route_decisions.len(), 1);
}

#[test]
fn preview_and_execution_share_exact_plan_without_preview_mutation() {
    let lock = route_lock(vec![FlowRoute {
        predicate: FlowPredicate::exists_artifact("items").unwrap(),
        for_each: Some(ArtifactRef::new("items").unwrap()),
        activate: "process".into(),
    }]);
    let mut driver = start_driver("run-preview-parity", RunMode::Continuous, 8);
    driver
        .runtime_mut()
        .apply_flow_lock(
            "run-preview-parity",
            FlowLockMode::FutureActivations,
            lock.id(),
            "hash",
        )
        .unwrap();
    driver
        .runtime_mut()
        .deliver_artifact("run-preview-parity", "root", "items", "a\nb")
        .unwrap();
    let before = driver.runtime().events().len();
    let preview = preview_flow_routes(driver.runtime().state(), "run-preview-parity", &lock)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(driver.runtime().events().len(), before);

    let report = driver.tick(DriverTickInput::default().with_route_lock(lock));
    assert_eq!(report.route_decisions.len(), 1);
    assert_eq!(report.route_decisions[0].route_id, preview.route_id);
    assert_eq!(
        report.route_decisions[0].planned_activation_ids,
        preview
            .planned_activations
            .iter()
            .map(|activation| activation.activation_id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        report.route_decisions[0].trigger,
        preview
            .trigger
            .expect("matched route should expose trigger")
    );
}

#[test]
fn complete_run_is_only_valid_from_quiescent_manual_state() {
    let mut driver = start_driver("run-complete", RunMode::Manual, 2);
    let early = driver.tick(
        DriverTickInput::default().with_control(ControlCommand::CompleteRun {
            run_id: "run-complete".into(),
        }),
    );
    assert_eq!(early.control_errors.len(), 1);
    assert_eq!(
        driver.runtime().state().run_status("run-complete"),
        Some(RunStatus::Running)
    );

    complete_activation(&mut driver, "run-complete", "root");
    assert_eq!(
        driver.runtime().state().run_status("run-complete"),
        Some(RunStatus::Quiescent)
    );
    let completed = driver.tick(DriverTickInput::default().with_control(
        ControlCommand::CompleteRun {
            run_id: "run-complete".into(),
        },
    ));
    assert!(completed.control_errors.is_empty());
    assert_eq!(
        driver.runtime().state().run_status("run-complete"),
        Some(RunStatus::Completed)
    );
}

#[test]
fn run_control_state_table_rejects_terminal_and_illegal_transitions() {
    for terminal in [RunStatus::Completed, RunStatus::Stopped, RunStatus::Failed] {
        for (control, expected_errors) in [
            (
                ControlCommand::StopRun {
                    run_id: "run-terminal".into(),
                },
                0,
            ),
            (
                ControlCommand::PauseRun {
                    run_id: "run-terminal".into(),
                },
                1,
            ),
            (
                ControlCommand::ResumeRun {
                    run_id: "run-terminal".into(),
                },
                1,
            ),
            (
                ControlCommand::CompleteRun {
                    run_id: "run-terminal".into(),
                },
                1,
            ),
        ] {
            let mut driver = start_driver("run-terminal", RunMode::Continuous, 4);
            driver
                .runtime_mut()
                .set_run_status("run-terminal", terminal)
                .unwrap();
            let before = driver.runtime().events().to_vec();

            let report = driver.tick(DriverTickInput::default().with_control(control));

            assert_eq!(
                report.control_errors.len(),
                expected_errors,
                "terminal={terminal:?}"
            );
            assert_eq!(driver.runtime().events(), before, "terminal={terminal:?}");
            assert_eq!(
                driver.runtime().state().run_status("run-terminal"),
                Some(terminal)
            );
        }
    }

    let mut running = start_driver("run-running", RunMode::Continuous, 4);
    let before = running.runtime().events().to_vec();
    let report = running.tick(
        DriverTickInput::default().with_control(ControlCommand::ResumeRun {
            run_id: "run-running".into(),
        }),
    );
    assert_eq!(report.control_errors.len(), 1);
    assert_eq!(running.runtime().events(), before);

    let mut exhausted = start_driver("run-exhausted", RunMode::Continuous, 4);
    exhausted
        .runtime_mut()
        .set_run_status_with_reason(
            "run-exhausted",
            RunStatus::Paused,
            Some("activation_limit_exhausted"),
        )
        .unwrap();
    let before = exhausted.runtime().events().to_vec();
    let report = exhausted.tick(DriverTickInput::default().with_control(
        ControlCommand::PauseRun {
            run_id: "run-exhausted".into(),
        },
    ));
    assert!(report.control_errors.is_empty());
    assert_eq!(exhausted.runtime().events(), before);
    assert_eq!(
        exhausted
            .runtime()
            .state()
            .run_status_reason("run-exhausted"),
        Some("activation_limit_exhausted")
    );

    let mut continuous = start_driver("run-continuous-complete", RunMode::Continuous, 4);
    complete_activation(&mut continuous, "run-continuous-complete", "root");
    assert_eq!(
        continuous
            .runtime()
            .state()
            .run_status("run-continuous-complete"),
        Some(RunStatus::Quiescent)
    );
    let before = continuous.runtime().events().to_vec();
    let report = continuous.tick(DriverTickInput::default().with_control(
        ControlCommand::CompleteRun {
            run_id: "run-continuous-complete".into(),
        },
    ));
    assert_eq!(report.control_errors.len(), 1);
    assert_eq!(continuous.runtime().events(), before);
}

#[test]
fn legacy_events_replay_with_finite_unbounded_generation_zero_defaults() {
    let legacy = json!([
        {
            "sequence": 1,
            "source": {
                "run_id": "legacy",
                "activation_id": null,
                "source_id": null
            },
            "kind": "run_started",
            "strength": "applied",
            "actor": "runtime",
            "correlation": null,
            "payload": {
                "type": "run_started",
                "run_id": "legacy"
            }
        },
        {
            "sequence": 2,
            "source": {
                "run_id": "legacy",
                "activation_id": "root",
                "source_id": null
            },
            "kind": "node_activated",
            "strength": "applied",
            "actor": "runtime",
            "correlation": null,
            "payload": {
                "type": "node_activated",
                "run_id": "legacy",
                "activation_id": "root",
                "node_id": "root",
                "stable_key": null,
                "context": {},
                "stop_contract": {
                    "required_artifacts": [],
                    "required_effects": []
                },
                "flow_lock_mode": null,
                "flow_lock_id": null,
                "contract_hash": null
            }
        }
    ]);
    let events = serde_json::from_value::<Vec<Event>>(legacy).unwrap();
    let state = RuntimeState::from_events(&events);

    assert_eq!(state.run_mode("legacy"), Some(RunMode::Finite));
    assert_eq!(state.activation_limit("legacy"), Some(u64::MAX));
    let activation = state
        .activations
        .get(&activation_key("legacy", "root"))
        .unwrap();
    assert_eq!(activation.activation_generation, 0);
    assert_eq!(activation.trigger, None);
    assert_eq!(activation.status, ActivationStatus::Pending);
}
