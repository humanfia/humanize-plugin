use super::*;

#[test]
fn bind_death_after_prompt_enter_exposes_ambiguity_without_resend() {
    let fixture = DriverFixture::new("driver-bind-prompt-enter-death");
    let run_id = "run-bind-prompt-enter-death";
    let crashing_tmux = fixture.fake_tmux_for_bind_crash("prompt_enter");
    let crashing_tmux_value = crashing_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        run_id,
        &[
            ("HUMANIZE_TMUX_BIN", &crashing_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "500"),
        ],
    );
    let request = fixture.initial_agent_bind_request(run_id);

    let response: Value =
        serde_json::from_str(&fixture.request_until_disconnect(request.clone())).unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["tmux"]["actuation"]["warnings"][0]["status"],
        "readiness_pending"
    );
    driver.wait_for_exit(Duration::from_secs(4));
    assert_eq!(fixture.agent_launch_count(), 1);
    assert_eq!(
        fixture.tmux_log_text().matches("Create the brief.").count(),
        1
    );

    let stable_tmux = fixture.fake_tmux(false);
    let stable_tmux_value = stable_tmux.to_string_lossy().to_string();
    let mut restarted =
        fixture.spawn_with_env(run_id, &[("HUMANIZE_TMUX_BIN", &stable_tmux_value)]);
    let recovered = fixture.request(request);
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(
        recovered["tmux"]["actuation"]["warnings"][0]["role"],
        "node_prompt"
    );
    assert_eq!(fixture.agent_launch_count(), 1);
    assert_eq!(
        fixture.tmux_log_text().matches("Create the brief.").count(),
        1
    );
    restarted.shutdown();
}

#[test]
fn prompt_death_exposes_barrier_and_submitted_resolution_survives_restart() {
    let fixture = DriverFixture::new("driver-prompt-death-ambiguity");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-prompt-death-ambiguity")
        .unwrap();
    let killing_tmux = fixture.fake_tmux_kills_driver_after_prompt_enter();
    let killing_tmux_value = killing_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-prompt-death-ambiguity",
        &[
            ("HUMANIZE_TMUX_BIN", &killing_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "500"),
        ],
    );
    fixture.bind(
        &mut driver,
        "run-prompt-death-ambiguity",
        manual_flow(NodeDriver::Agent),
        Some(fixture.tmux_request()),
    );

    let response: Value = serde_json::from_str(&fixture.request_until_disconnect(json!({
        "id": "activate-agent-before-prompt-death",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-prompt-death-ambiguity",
        "node_id": "manual"
    })))
    .unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["actuation"]["warnings"][0]["status"],
        "readiness_pending"
    );
    driver.wait_for_exit(Duration::from_secs(4));
    assert_eq!(fixture.agent_launch_count(), 1);
    assert_eq!(
        fixture
            .tmux_log_text()
            .matches("Inspect the manual node.")
            .count(),
        1
    );

    let stable_tmux = fixture.fake_tmux(false);
    let stable_tmux_value = stable_tmux.to_string_lossy().to_string();
    let mut restarted = fixture.spawn_with_env(
        "run-prompt-death-ambiguity",
        &[
            ("HUMANIZE_TMUX_BIN", &stable_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
        ],
    );
    let resumed = fixture.request(json!({
        "id": "resume-prompt-ambiguity",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-prompt-death-ambiguity"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(resumed["actuation"]["warnings"][0]["role"], "node_prompt");
    assert_eq!(fixture.agent_launch_count(), 1);
    assert_eq!(
        fixture
            .tmux_log_text()
            .matches("Inspect the manual node.")
            .count(),
        1
    );
    let ambiguous = fixture.status("run-prompt-death-ambiguity");
    let started_event_sequence =
        ambiguous["context"]["ambiguous_deliveries"][0]["started_event_sequence"]
            .as_u64()
            .unwrap();

    let resolved = fixture.request(json!({
        "id": "resolve-prompt-as-submitted",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-prompt-death-ambiguity",
        "delivery_resolution": {
            "started_event_sequence": started_event_sequence,
            "outcome": "submitted",
            "evidence": "receiver acknowledged the prompt transaction"
        }
    }));
    assert_eq!(resolved["ok"], true, "{resolved}");
    assert_eq!(
        resolved["delivery_resolution"]["started_event_sequence"],
        started_event_sequence
    );
    assert_eq!(resolved["delivery_resolution"]["role"], "node_prompt");
    assert_eq!(resolved["delivery_resolution"]["outcome"], "submitted");
    assert_eq!(
        fixture.status("run-prompt-death-ambiguity")["context"]["ambiguous_deliveries"],
        json!([])
    );
    restarted.crash();

    let calls_before_second_restart = fixture.tmux_log_text();
    let mut replayed = fixture.spawn_with_env(
        "run-prompt-death-ambiguity",
        &[("HUMANIZE_TMUX_BIN", &stable_tmux_value)],
    );
    assert_eq!(
        fixture.status("run-prompt-death-ambiguity")["context"]["ambiguous_deliveries"],
        json!([])
    );
    let resumed_after_restart = fixture.request(json!({
        "id": "resume-after-submitted-resolution-replay",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-prompt-death-ambiguity"
    }));
    assert_eq!(resumed_after_restart["ok"], true, "{resumed_after_restart}");
    let input_before = calls_before_second_restart
        .lines()
        .filter(|line| {
            line.starts_with("set-buffer ")
                || line.starts_with("paste-buffer ")
                || line.starts_with("send-keys ")
        })
        .collect::<Vec<_>>();
    let calls_after_second_restart = fixture.tmux_log_text();
    let input_after = calls_after_second_restart
        .lines()
        .filter(|line| {
            line.starts_with("set-buffer ")
                || line.starts_with("paste-buffer ")
                || line.starts_with("send-keys ")
        })
        .collect::<Vec<_>>();
    assert_eq!(input_after, input_before);
    replayed.shutdown();
}

#[test]
fn case_variant_acceptance_marker_confirms_prompt_delivery() {
    let fixture = DriverFixture::new("driver-case-variant-acceptance");
    let fake_tmux = fixture.fake_tmux(true);
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-case-variant-acceptance",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
            ("HUMANIZE_TEST_CODEX_CAPTURE", "case_variant"),
        ],
    );
    fixture.bind(
        &mut driver,
        "run-case-variant-acceptance",
        manual_flow(NodeDriver::Agent),
        Some(fixture.tmux_request_with_agent_command("codex")),
    );

    let started = fixture.request(json!({
        "id": "activate-case-variant-agent",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-case-variant-acceptance",
        "node_id": "manual"
    }));
    assert_eq!(
        started["actuation"]["warnings"][0]["status"], "readiness_pending",
        "{started}"
    );
    assert_eq!(
        fixture.status("run-case-variant-acceptance")["context"]["ambiguous_deliveries"],
        json!([])
    );
    let events = fs::read_to_string(
        fixture
            .private_driver_dir("run-case-variant-acceptance")
            .join("driver-events.jsonl"),
    )
    .unwrap();
    let prompt_submission = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| {
            event["kind"] == "prompt_submitted" && event["payload"]["activation_id"] == "manual"
        })
        .unwrap();
    assert_eq!(
        prompt_submission["payload"]["acceptance"],
        json!({ "profile": "codex", "signal": "working_state" })
    );
    driver.shutdown();
}

#[test]
fn missing_acceptance_marker_after_enter_commits_prompt_without_duplicate_resend() {
    let fixture = DriverFixture::new("driver-missing-acceptance-marker");
    let fake_tmux = fixture.fake_tmux(true);
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env(
        "run-missing-acceptance-marker",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "0"),
            ("HUMANIZE_TEST_CODEX_CAPTURE", "missing_marker"),
        ],
    );
    fixture.bind(
        &mut driver,
        "run-missing-acceptance-marker",
        manual_flow(NodeDriver::Agent),
        Some(fixture.tmux_request_with_agent_command("codex")),
    );

    let started = fixture.request(json!({
        "id": "activate-missing-marker-agent",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-missing-acceptance-marker",
        "node_id": "manual"
    }));
    assert_eq!(
        started["actuation"]["warnings"][0]["status"], "readiness_pending",
        "{started}"
    );
    assert_eq!(
        fixture.status("run-missing-acceptance-marker")["context"]["ambiguous_deliveries"],
        json!([])
    );
    let events = fs::read_to_string(
        fixture
            .private_driver_dir("run-missing-acceptance-marker")
            .join("driver-events.jsonl"),
    )
    .unwrap();
    let prompt_submission = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| {
            event["kind"] == "prompt_submitted" && event["payload"]["activation_id"] == "manual"
        })
        .unwrap();
    assert_eq!(prompt_submission["payload"]["acceptance"], Value::Null);

    let input_before_reconciliation = fixture
        .tmux_log_text()
        .matches("Inspect the manual node.")
        .count();
    let reconciled = fixture.request(json!({
        "id": "patch-after-missing-marker-submission",
        "token": fixture.token,
        "op": "patch_board",
        "run_id": "run-missing-acceptance-marker",
        "activation_id": "manual",
        "patch": { "recovery_probe": "no_resend" }
    }));
    assert_eq!(reconciled["ok"], true, "{reconciled}");
    assert_eq!(
        fixture
            .tmux_log_text()
            .matches("Inspect the manual node.")
            .count(),
        input_before_reconciliation
    );
    assert_eq!(
        fixture.status("run-missing-acceptance-marker")["context"]["ambiguous_deliveries"],
        json!([])
    );
    driver.shutdown();
}
