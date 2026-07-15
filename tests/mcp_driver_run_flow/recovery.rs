use super::*;

#[test]
fn prompt_receipt_failure_replays_ambiguity_without_resend_until_resolution() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-prompt-recovery")
        .join(std::process::id().to_string());
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    set_test_state_root(&root);
    let tmux_control = ControlledTmuxFixture::new(&root);
    let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
    let prior_driver_event_fault = std::env::var_os("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS");
    let prior_driver_event_kind = std::env::var_os("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND");
    let fake_tmux = fake_tmux_with_sequential_panes(&tmux_control);
    let fault_marker = root.join("fail-prompt-event");
    fs::write(&fault_marker, "fail").unwrap();
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
        std::env::set_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", &fault_marker);
        std::env::set_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND", "prompt_submitted");
    }

    let mut server = McpServer::with_tmux_runner_run_asset_store_and_execution_defaults(
        SystemCommandRunner,
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs"))),
        TmuxExecutionDefaults {
            session: Some("host-a".into()),
            window: Some("flow-a".into()),
            agent_command: Some("humanize-test-agent".into()),
        },
    );
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-driver-prompt-recovery",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(tmux_control.wait_for_hooks());

    assert_eq!(structured(&started)["ok"], true, "{started}");
    assert_eq!(
        structured(&started)["tmux"]["actuation"]["warnings"][0]["status"],
        "readiness_pending"
    );
    let calls_before_restart = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert_eq!(
        calls_before_restart.matches("humanize-test-agent").count(),
        1
    );
    assert_eq!(
        calls_before_restart
            .matches("Inspect the repository.")
            .count(),
        1
    );

    let driver_pid = wait_for_file(&root.join("driver.pid"))
        .trim()
        .parse::<i32>()
        .unwrap();
    assert_eq!(unsafe { libc::kill(driver_pid, libc::SIGKILL) }, 0);
    wait_for_process_exit(driver_pid);
    fs::remove_file(&fault_marker).unwrap();

    let retried = call_tool(
        &mut server,
        3,
        "run_flow",
        json!({
            "run_id": "run-driver-prompt-recovery",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert_eq!(structured(&retried)["ok"], true, "{retried}");
    assert_eq!(structured(&retried)["run_status"], "paused", "{retried}");

    let context = call_tool(
        &mut server,
        4,
        "get_context",
        json!({ "run_id": "run-driver-prompt-recovery" }),
    );
    let barriers = structured(&context)["context"]["ambiguous_deliveries"]
        .as_array()
        .unwrap();
    assert_eq!(barriers.len(), 1, "{context}");
    assert_eq!(barriers[0]["role"], "node_prompt", "{context}");
    let prompt_sequence = barriers[0]["started_event_sequence"].as_u64().unwrap();
    let calls_after_restart = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert_eq!(
        calls_after_restart.matches("humanize-test-agent").count(),
        1
    );
    assert_eq!(
        calls_after_restart
            .matches("Inspect the repository.")
            .count(),
        1
    );

    let resumed = call_tool(
        &mut server,
        5,
        "resume_run",
        json!({
            "run_id": "run-driver-prompt-recovery",
            "delivery_resolution": {
                "started_event_sequence": prompt_sequence,
                "outcome": "not_submitted",
                "evidence": "receiver transcript confirms the prompt was not retained"
            }
        }),
    );
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);
    restore_env(
        "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS",
        prior_driver_event_fault,
    );
    restore_env(
        "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
        prior_driver_event_kind,
    );

    assert_eq!(structured(&resumed)["ok"], true, "{resumed}");
    assert_eq!(structured(&resumed)["run_status"], "running", "{resumed}");
    let calls_after_resolution = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert_eq!(
        calls_after_resolution
            .matches("humanize-test-agent")
            .count(),
        1
    );
    assert_eq!(
        calls_after_resolution
            .matches("Inspect the repository.")
            .count(),
        2
    );
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-driver-prompt-recovery")
        .unwrap();
    shutdown_driver_for_run(&run_root, "run-driver-prompt-recovery");
}
