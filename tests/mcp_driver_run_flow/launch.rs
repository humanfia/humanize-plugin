use super::*;

#[test]
fn run_flow_launches_driver_pane_and_binds_locked_flow_through_ipc() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-run-flow")
        .join(std::process::id().to_string());
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    set_test_state_root(&root);
    let tmux_control = ControlledTmuxFixture::new(&root);
    let fake_tmux = fake_tmux(&root);
    let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
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
            "run_id": "run-driver-flow",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(tmux_control.wait_for_hooks());

    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);
    assert_eq!(structured(&started)["ok"], true);
    assert!(structured(&started)["event_cursor"].as_u64().unwrap() > 0);
    assert!(structured(&started)["context_generation"].as_u64().unwrap() > 0);
    assert!(structured(&started).get("driver").is_none());
    assert!(!structured(&started).to_string().contains("%8"));
    assert_eq!(
        structured(&started)["tmux"]["panes"][0]["activation_id"],
        "root"
    );
    let calls = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert!(calls.contains("new-session -d -P -F #{window_id}|#{pane_id} -s host-a -n flow-a"));
    assert!(calls.contains("set-buffer -b machine-input-"));
    assert!(calls.contains("paste-buffer -p -d -b machine-input-"));
    assert!(calls.contains("humanize-plugin-driver"));
    assert!(calls.contains("send-keys -t host-a:%7.%9 -l env HUMANIZE_PARTICIPANT_BINDING_FILE="));
    assert!(calls.contains("humanize-test-agent"));
    assert!(calls.contains("send-keys -t host-a:%7.%9 C-u"));
    assert!(calls.contains("send-keys -t host-a:%7.%9 Enter"));
    assert!(calls.contains("Inspect the repository.\n\nResources:\n- Use Humanize to audit this library without editing files."));
    assert!(!calls.contains("README.md"));
    assert!(!calls.contains("commands: help status"));
    assert!(!calls.contains("send-keys -t host-a:%7.%8 -l env HUMANIZE_PARTICIPANT_BINDING_FILE="));
    assert!(!calls.contains("HUMANIZE_PARTICIPANT_CREDENTIAL="));
    assert!(calls.contains("pipe-pane -o -t host-a:%7.%9"));
    assert!(!calls.contains("capture-pane"));
    assert!(!calls.contains("kill-pane"));

    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({
            "run_id": "run-driver-flow"
        }),
    );
    assert_eq!(structured(&status)["ok"], true);
    assert_eq!(structured(&status)["run_status"], "running");
    assert_eq!(
        structured(&status)["event_cursor"],
        structured(&started)["event_cursor"]
    );
    assert!(
        structured(&status)["context_generation"].as_u64().unwrap()
            >= structured(&started)["context_generation"].as_u64().unwrap()
    );
    let mut second_server = McpServer::with_tmux_runner_run_asset_store_and_execution_defaults(
        SystemCommandRunner,
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs"))),
        TmuxExecutionDefaults {
            session: Some("host-a".into()),
            window: Some("flow-a".into()),
            agent_command: Some("humanize-test-agent".into()),
        },
    );
    let second_status = call_tool(
        &mut second_server,
        4,
        "run_status",
        json!({
            "run_id": "run-driver-flow"
        }),
    );
    assert_eq!(structured(&second_status)["ok"], true);
    assert_eq!(
        structured(&second_status)["event_cursor"],
        structured(&started)["event_cursor"]
    );
    assert_eq!(
        structured(&second_status)["context_generation"],
        structured(&status)["context_generation"]
    );
    let sent = call_tool(
        &mut server,
        5,
        "send_message",
        json!({
            "run_id": "run-driver-flow",
            "activation_id": "root",
            "message_id": "message-1",
            "text": "registry-targeted-message"
        }),
    );
    assert_eq!(structured(&sent)["ok"], true, "{sent}");
    assert_eq!(structured(&sent)["receipt"]["status"], "submitted");
    assert_eq!(structured(&sent)["receipt"]["message_id"], "message-1");
    let replayed = call_tool(
        &mut second_server,
        6,
        "send_message",
        json!({
            "runId": "run-driver-flow",
            "activationId": "root",
            "messageId": "message-1",
            "message": "registry-targeted-message"
        }),
    );
    assert_eq!(structured(&replayed)["ok"], true, "{replayed}");
    assert_eq!(
        structured(&replayed)["receipt"],
        structured(&sent)["receipt"]
    );
    let conflict = call_tool(
        &mut second_server,
        7,
        "send_message",
        json!({
            "run_id": "run-driver-flow",
            "activation_id": "root",
            "message_id": "message-1",
            "text": "conflicting-targeted-message"
        }),
    );
    assert_eq!(structured(&conflict)["ok"], false, "{conflict}");
    assert_eq!(
        structured(&conflict)["error"]["code"],
        "message_id_conflict"
    );
    let message_calls = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert_eq!(
        message_calls.matches("registry-targeted-message").count(),
        1
    );
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-driver-flow")
        .unwrap();
    let manifest = private_run_assets(&run_root);
    assert_eq!(manifest["flow"]["status"], "complete");
    assert_eq!(manifest["flow"]["complete"], true);
    let driver_dir = private_driver_dir(&run_root);
    let driver_events = fs::read_to_string(driver_dir.join("driver-events.jsonl")).unwrap();
    assert!(driver_events.contains("\"kind\":\"driver_pane_owned\""));
    assert!(driver_events.contains("\"pane_id\":\"%8\""));
    assert_eq!(
        manifest["activations"]["root"]["capture_phase"],
        "capturing"
    );
    assert_eq!(manifest["activations"]["root"]["pipe_acknowledged"], true);
    assert_eq!(
        manifest["activations"]["root"]["resource_cleanup_status"],
        "owned"
    );
    let transcript = manifest["activations"]["root"]["pipe_path"]
        .as_str()
        .unwrap();
    assert!(fs::metadata(transcript).unwrap().len() > 0);
    assert!(Path::new(transcript).starts_with(driver_dir.parent().unwrap()));
    assert!(!run_root.join("activations").exists());
    assert!(!run_root.join("driver").exists());
    assert_eq!(file_mode(&driver_dir), 0o700);
    assert_eq!(file_mode(driver_dir.join("ipc-token")), 0o600);
    assert_eq!(file_mode(driver_dir.join("ipc.json")), 0o600);
    let token = fs::read_to_string(driver_dir.join("ipc-token"))
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(driver_dir.join("ipc.json")).unwrap()).unwrap();
    assert!(metadata.get("auth_token").is_none());
    assert_eq!(metadata["auth_token_path"], "ipc-token");
    let readiness_nonce = manifest["activations"]["root"]["readiness_nonce"]
        .as_str()
        .unwrap()
        .to_string();
    let binding = read_single_private_binding(driver_dir.parent().unwrap());
    let participant_handle = binding["handle"].as_str().unwrap().to_string();
    let participant_credential = binding["credential"].as_str().unwrap().to_string();
    let binding_path = binding["__path"].as_str().unwrap().to_string();

    let stopped = call_tool(
        &mut second_server,
        8,
        "stop_run",
        json!({ "run_id": "run-driver-flow" }),
    );
    assert_eq!(structured(&stopped)["ok"], true, "{stopped}");
    let stopped_manifest = private_run_assets(&run_root);
    assert_eq!(
        stopped_manifest["activations"]["root"]["capture_phase"],
        "complete"
    );
    assert_eq!(
        stopped_manifest["activations"]["root"]["resource_cleanup_status"],
        "complete"
    );
    let final_capture = stopped_manifest["activations"]["root"]["final_capture_path"]
        .as_str()
        .unwrap();
    assert!(fs::metadata(final_capture).unwrap().len() > 0);
    let stopped_calls = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert!(stopped_calls.contains("capture-pane -p -t host-a:%7.%9"));
    assert!(stopped_calls.contains("kill-pane -t host-a:%7.%9"));
    assert!(!stopped_calls.contains("kill-pane -t host-a:%7.%8"));

    shutdown_driver(&run_root);
    let shutdown_calls = wait_for_text(&root.join("tmux.log"), "kill-pane -t host-a:%7.%8");
    assert!(shutdown_calls.contains("kill-pane -t host-a:%7.%8"));
    let shutdown_events = wait_for_text(
        &private_driver_dir(&run_root).join("driver-events.jsonl"),
        "\"kind\":\"driver_pane_released\"",
    );
    assert!(shutdown_events.contains("\"pane_id\":\"%8\""));

    let public_files = collect_public_files(&run_root);
    assert_eq!(
        public_files,
        BTreeSet::from([
            "flow/revisions/rev-0001/flow-lock.json".to_string(),
            "manifest.json".to_string(),
            "records/events.jsonl".to_string(),
            "records/journal-seal.json".to_string(),
        ])
    );
    let mut observable = serde_json::to_vec(&json!([
        structured(&started).clone(),
        structured(&status).clone(),
        structured(&second_status).clone(),
        structured(&sent).clone(),
        structured(&replayed).clone(),
        structured(&conflict).clone(),
        structured(&stopped).clone(),
    ]))
    .unwrap();
    observable.extend(collect_tree_bytes(&run_root));
    for forbidden in [
        root.to_string_lossy().into_owned(),
        run_root.to_string_lossy().into_owned(),
        driver_dir.parent().unwrap().to_string_lossy().into_owned(),
        transcript.to_string(),
        final_capture.to_string(),
        binding_path,
        token,
        readiness_nonce,
        participant_handle,
        participant_credential,
        "host-a".to_string(),
        "flow-a".to_string(),
        "%7".to_string(),
        "%8".to_string(),
        "%9".to_string(),
        "fake-native-9".to_string(),
        "humanize-test-agent".to_string(),
        "registry-targeted-message".to_string(),
    ] {
        assert!(
            !contains_bytes(&observable, forbidden.as_bytes()),
            "observable output leaked {forbidden}"
        );
    }
}
