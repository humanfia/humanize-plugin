use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use super::driver_flows::{
    later_agent_flow, parallel_agent_flow, reviewed_lock_package, routed_locked_flow,
};
use super::support::DriverFixture;

#[test]
fn agent_launch_uses_private_binding_file_without_exposing_capability() {
    let fixture = DriverFixture::new("participant-started-before-launch");
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")));
    store.start_run_manifest("run-participant-start").unwrap();
    let fake_tmux = fixture.fake_tmux_without_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env_values(
        "run-participant-start",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "25"),
        ],
    );

    let bound = fixture.request(json!({
        "id": "bind",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-participant-start",
        "flow_lock": later_agent_flow(),
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        },
        "review": {"review_id":"review", "status":"approved"}
    }));
    assert_eq!(bound["ok"], true, "{bound}");

    let activated = fixture.request(json!({
        "id": "activate",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-participant-start",
        "node_id": "manual"
    }));
    assert_eq!(activated["ok"], true, "{activated}");

    let events = fs::read_to_string(fixture.driver_events_path("run-participant-start")).unwrap();
    let binding = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| {
            event["kind"] == "participant_started"
                && event["payload"]["binding"]["activation_id"] == "manual"
        })
        .map(|event| event["payload"]["binding"].clone())
        .expect("participant binding event must exist");
    assert_eq!(binding["handle"].as_str().unwrap().len(), 64);
    assert!(binding.get("credential").is_none());
    let binding_relative = binding["binding_file"]
        .as_str()
        .expect("binding event must reference its private file");
    assert!(!Path::new(binding_relative).is_absolute());
    let binding_path = fixture
        .private_run_root("run-participant-start")
        .join(binding_relative);
    let metadata = fs::metadata(&binding_path).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(metadata.nlink(), 1);
    let private_binding: Value = serde_json::from_slice(&fs::read(&binding_path).unwrap()).unwrap();
    let credential = private_binding["credential"].as_str().unwrap();
    assert_eq!(credential.len(), 64);
    assert!(!events.contains(credential));
    let started = events
        .lines()
        .position(|line| line.contains("\"kind\":\"participant_started\""))
        .expect("participant binding must be durable before launch");
    let launch = events
        .lines()
        .position(|line| line.contains("\"role\":\"agent_launch\""))
        .expect("agent launch intent must be durable");
    assert!(started < launch);

    let tmux = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert!(
        tmux.contains("HUMANIZE_PARTICIPANT_BINDING_FILE="),
        "{tmux}"
    );
    assert!(!tmux.contains("HUMANIZE_PARTICIPANT_CREDENTIAL="), "{tmux}");
    assert!(!tmux.contains(credential), "{tmux}");
    assert!(tmux.contains("--participant-exited-hook"), "{tmux}");
    let run_root = store.run_root("run-participant-start").unwrap();
    assert_tree_does_not_contain(&run_root, credential);
    let ledger = fs::read_to_string(
        fixture
            .private_driver_dir("run-participant-start")
            .join("machine-inputs.jsonl"),
    )
    .unwrap();
    assert!(ledger.contains("participant-agent-launch"), "{ledger}");
    assert!(!ledger.contains(credential));
    driver.shutdown();
}

#[test]
fn stop_attempt_limit_is_bounded_and_frozen_by_initial_bind() {
    let fixture = DriverFixture::new("participant-stop-limit-bind");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-stop-limit")
        .unwrap();
    let mut driver = fixture.spawn("run-stop-limit");
    let rejected = fixture.request(json!({
        "id":"bind-invalid",
        "token":fixture.token,
        "op":"bind_run",
        "run_id":"run-stop-limit",
        "flow_lock":routed_locked_flow(),
        "stop_attempt_limit":9,
        "review":{"review_id":"review","status":"approved"}
    }));
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "malformed_request");

    let bound = fixture.request(json!({
        "id":"bind-valid",
        "token":fixture.token,
        "op":"bind_run",
        "run_id":"run-stop-limit",
        "flow_lock":routed_locked_flow(),
        "stop_attempt_limit":2,
        "review":{"review_id":"review","status":"approved"}
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    assert_eq!(bound["stop_attempt_limit"], 2);
    let conflict = fixture.request(json!({
        "id":"bind-conflict",
        "token":fixture.token,
        "op":"bind_run",
        "run_id":"run-stop-limit",
        "flow_lock":routed_locked_flow(),
        "stop_attempt_limit":3,
        "review":{"review_id":"review","status":"approved"}
    }));
    assert_eq!(conflict["ok"], false, "{conflict}");
    assert_eq!(conflict["error"]["code"], "run_binding_conflict");
    driver.shutdown();
}

#[test]
fn native_binding_is_authoritative_and_manifest_readiness_cannot_submit_prompt() {
    let fixture = DriverFixture::new("participant-native-binding");
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")));
    store.start_run_manifest("run-participant-bind").unwrap();
    let fake_tmux = fixture.fake_tmux_without_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env_values(
        "run-participant-bind",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "25"),
        ],
    );
    bind_and_activate_agent(&fixture, "run-participant-bind");
    let binding = participant_binding(&fixture, "run-participant-bind");

    fs::write(
        fixture
            .run_root("run-participant-bind")
            .join("manifest.json"),
        serde_json::to_vec(&json!({
            "status": "ready",
            "activations": {
                "manual": {
                    "pane_id": binding["pane_id"],
                    "allocation_generation": binding["allocation_generation"],
                    "readiness_nonce": binding["readiness_nonce"]
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    thread::sleep(Duration::from_millis(50));
    let before_binding = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert!(!before_binding.contains("Inspect the manual node."));

    let request = participant_bind_request(&binding, fixture.token, "run-participant-bind");
    let first = fixture.request(request.clone());
    assert_eq!(first["ok"], true, "{first}");
    let second = fixture.request(request);
    assert_eq!(second["ok"], true, "{second}");
    let events = fs::read_to_string(fixture.driver_events_path("run-participant-bind")).unwrap();
    assert_eq!(events.matches("\"kind\":\"participant_bound\"").count(), 1);
    assert_eq!(
        public_event_kind_count(&fixture, "run-participant-bind", "agent_session.started"),
        1
    );
    assert_eq!(
        public_event_kind_count(&fixture, "run-participant-bind", "agent_session.bound"),
        1
    );
    wait_for(Duration::from_secs(2), || {
        fs::read_to_string(fixture.tmux_log())
            .is_ok_and(|log| log.contains("Inspect the manual node."))
    });
    let tmux = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert_eq!(
        tmux.matches("Inspect the manual node.").count(),
        1,
        "{tmux}"
    );
    driver.shutdown();
}

#[test]
fn codex_and_claude_session_start_hooks_bind_native_sessions_silently() {
    for (platform, source) in [
        ("codex", "codex_session_start"),
        ("claude", "claude_session_start"),
    ] {
        let run_id = format!("run-{platform}-session-start");
        let fixture = DriverFixture::new(&format!("participant-session-start-{platform}"));
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
            .start_run_manifest(&run_id)
            .unwrap();
        let fake_tmux = fixture.fake_tmux_without_agent_ready();
        let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
        let mut driver = fixture.spawn_with_env_values(
            &run_id,
            &[
                ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
                ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "25"),
            ],
        );
        bind_and_activate_agent(&fixture, &run_id);
        let binding = participant_binding(&fixture, &run_id);

        let mut child = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
            .arg("--agent-ready-hook")
            .arg("--source")
            .arg(source)
            .env(
                "HUMANIZE_PARTICIPANT_BINDING_FILE",
                binding["binding_file_path"].as_str().unwrap(),
            )
            .env("TMUX_PANE", binding["pane_id"].as_str().unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(
            child.stdin.as_mut().unwrap(),
            "{}",
            json!({
                "hook_event_name":"SessionStart",
                "session_id":format!("native-{platform}-session")
            })
        )
        .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());

        let events = fs::read_to_string(fixture.driver_events_path(&run_id)).unwrap();
        let bound = events
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["kind"] == "participant_bound")
            .unwrap();
        assert_eq!(bound["payload"]["platform"], platform);
        assert_eq!(bound["payload"]["source"], source);
        assert_eq!(
            bound["payload"]["native_session_id"],
            format!("native-{platform}-session")
        );
        driver.shutdown();
    }
}

#[test]
fn participant_mcp_injects_binding_and_driver_enforces_scoped_authority() {
    let fixture = DriverFixture::new("participant-mcp-authority");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-participant-mcp")
        .unwrap();
    let fake_tmux = fixture.fake_tmux_without_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env_values(
        "run-participant-mcp",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "25"),
        ],
    );
    bind_and_activate_agent(&fixture, "run-participant-mcp");
    let binding = participant_binding(&fixture, "run-participant-mcp");
    let bound = fixture.request(participant_bind_request(
        &binding,
        fixture.token,
        "run-participant-mcp",
    ));
    assert_eq!(bound["ok"], true, "{bound}");

    let responses = participant_mcp_requests(
        &fixture,
        &binding,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_context","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"deliver_artifact","arguments":{"artifact_key":"participant_note","payload":"ready"}}}),
        ],
        None,
    );
    let context = &responses[0]["result"]["structuredContent"];
    assert_eq!(context["ok"], true, "{}", responses[0]);
    assert!(context.get("run_id").is_none(), "{context}");
    assert!(context["context"]["activations"].get("root").is_none());
    assert!(context["context"]["activations"].get("manual").is_some());
    assert!(context["context"].get("run_assets").is_none());
    assert_eq!(
        responses[1]["result"]["structuredContent"]["artifact_key"],
        "participant_note"
    );

    let rejected = participant_mcp_requests(
        &fixture,
        &binding,
        &[
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"validate_stop","arguments":{}}}),
        ],
        Some("wrong-credential"),
    );
    assert_eq!(
        rejected[0]["result"]["structuredContent"]["error"]["code"],
        "participant_unauthorized"
    );
    driver.shutdown();
}

#[test]
fn stop_invocations_are_idempotent_bounded_and_replayed() {
    let fixture = DriverFixture::new("participant-stop-replay");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-participant-stop")
        .unwrap();
    let fake_tmux = fixture.fake_tmux_without_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let driver_env = [
        ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
        ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "25"),
    ];
    let mut driver = fixture.spawn_with_env_values("run-participant-stop", &driver_env);
    let bound = fixture.request(json!({
        "id":"bind",
        "token":fixture.token,
        "op":"bind_run",
        "run_id":"run-participant-stop",
        "flow_lock":reviewed_lock_package(),
        "stop_attempt_limit":2,
        "tmux":{
            "enabled":true,
            "session":"host-a",
            "window":"flow-a",
            "agent_command":"humanize-test-agent"
        },
        "review":{"review_id":"review","status":"approved"}
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    let binding = participant_binding_for(&fixture, "run-participant-stop", "root");
    let participant_bound = fixture.request(participant_bind_request(
        &binding,
        fixture.token,
        "run-participant-stop",
    ));
    assert_eq!(participant_bound["ok"], true, "{participant_bound}");

    let first_request = participant_stop_request(
        &binding,
        fixture.token,
        "run-participant-stop",
        "stop-invocation-1",
    );
    let first = fixture.request(first_request.clone());
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["decision"], "deny");
    assert_eq!(first["attempt"], 1);
    assert_eq!(first["hook_action"], "deny");
    let duplicate = fixture.request(first_request);
    assert_eq!(duplicate["decision"], "deny");
    assert_eq!(duplicate["attempt"], 1);
    assert_eq!(duplicate["event_cursor"], first["event_cursor"]);
    assert_eq!(duplicate["idempotent"], true);

    driver.crash();
    let mut restarted = fixture.spawn_with_env_values("run-participant-stop", &driver_env);
    let second = fixture.request(participant_stop_request(
        &binding,
        fixture.token,
        "run-participant-stop",
        "stop-invocation-2",
    ));
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(second["decision"], "block");
    assert_eq!(second["attempt"], 2);
    assert_eq!(second["hook_action"], "allow");
    assert_eq!(second["activation_status"], "blocked");
    let before_exit = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert!(!before_exit.contains("kill-pane -t host-a:%7.%8"));
    let exited = run_participant_exit_hook(&fixture, &binding, 0);
    assert!(
        exited.status.success(),
        "{}",
        String::from_utf8_lossy(&exited.stderr)
    );
    wait_for(Duration::from_secs(2), || {
        fs::read_to_string(fixture.tmux_log())
            .is_ok_and(|log| log.contains("kill-pane -t host-a:%7.%8"))
    });
    restarted.shutdown();
}

#[test]
fn codex_and_claude_stop_hooks_map_deny_and_allow_without_internal_ids() {
    let official_block: Value =
        serde_json::from_str(include_str!("../fixtures/hooks/stop_block.json")).unwrap();
    let official_allow: Value =
        serde_json::from_str(include_str!("../fixtures/hooks/stop_allow.json")).unwrap();
    for (platform, flag, source) in [
        ("codex", "--codex-stop-hook", "codex_session_start"),
        ("claude", "--claude-stop-hook", "claude_session_start"),
    ] {
        let run_id = format!("run-{platform}-stop-hook");
        let fixture = DriverFixture::new(&format!("participant-stop-hook-{platform}"));
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
            .start_run_manifest(&run_id)
            .unwrap();
        let fake_tmux = fixture.fake_tmux_without_agent_ready();
        let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
        let mut driver = fixture.spawn_with_env_values(
            &run_id,
            &[
                ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
                ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "25"),
            ],
        );
        let bound = fixture.request(json!({
            "id":"bind",
            "token":fixture.token,
            "op":"bind_run",
            "run_id":run_id,
            "flow_lock":reviewed_lock_package(),
            "tmux":{"enabled":true,"session":"host-a","window":"flow-a","agent_command":"humanize-test-agent"},
            "review":{"review_id":"review","status":"approved"}
        }));
        assert_eq!(bound["ok"], true, "{bound}");
        let binding = participant_binding_for(&fixture, &run_id, "root");
        let participant_bound = fixture.request(participant_bind_request_for_platform(
            &binding,
            fixture.token,
            &run_id,
            platform,
            source,
        ));
        assert_eq!(participant_bound["ok"], true, "{participant_bound}");

        let denied = run_stop_hook(
            &fixture,
            &binding,
            flag,
            &json!({
                "hook_event_name":"Stop",
                "session_id":format!("native-{platform}-session"),
                "hook_id":"stop-denied"
            }),
        );
        let denial_text = denied.to_string();
        assert!(denial_text.contains("brief"), "{denied}");
        for forbidden in [&run_id, "root", "%8", "flk_"] {
            assert!(!denial_text.contains(forbidden), "{denied}");
        }
        assert_eq!(denied, official_block);

        let delivered = fixture.request(json!({
            "id":"deliver",
            "token":fixture.token,
            "op":"deliver_artifact",
            "run_id":run_id,
            "activation_id":"root",
            "artifact_id":"brief",
            "payload":"done"
        }));
        assert_eq!(delivered["ok"], true, "{delivered}");
        let allowed = run_stop_hook(
            &fixture,
            &binding,
            flag,
            &json!({
                "hook_event_name":"Stop",
                "session_id":format!("native-{platform}-session"),
                "hook_id":"stop-allowed"
            }),
        );
        assert_eq!(allowed, official_allow);
        driver.shutdown();
    }
}

#[test]
fn codex_and_claude_stop_hooks_count_same_native_payload_as_distinct_invocations() {
    let official_block: Value =
        serde_json::from_str(include_str!("../fixtures/hooks/stop_block.json")).unwrap();
    let official_allow: Value =
        serde_json::from_str(include_str!("../fixtures/hooks/stop_allow.json")).unwrap();
    for (platform, flag, source) in [
        ("codex", "--codex-stop-hook", "codex_session_start"),
        ("claude", "--claude-stop-hook", "claude_session_start"),
    ] {
        let run_id = format!("run-{platform}-stop-no-native-id");
        let fixture = DriverFixture::new(&format!("p-stop-no-id-{platform}"));
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
            .start_run_manifest(&run_id)
            .unwrap();
        let fake_tmux = fixture.fake_tmux_without_agent_ready();
        let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
        let mut driver = fixture.spawn_with_env_values(
            &run_id,
            &[
                ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
                ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "25"),
            ],
        );
        let bound = fixture.request(json!({
            "id":"bind",
            "token":fixture.token,
            "op":"bind_run",
            "run_id":run_id,
            "flow_lock":reviewed_lock_package(),
            "stop_attempt_limit":2,
            "tmux":{"enabled":true,"session":"host-a","window":"flow-a","agent_command":"humanize-test-agent"},
            "review":{"review_id":"review","status":"approved"}
        }));
        assert_eq!(bound["ok"], true, "{bound}");
        let binding = participant_binding_for(&fixture, &run_id, "root");
        let participant_bound = fixture.request(participant_bind_request_for_platform(
            &binding,
            fixture.token,
            &run_id,
            platform,
            source,
        ));
        assert_eq!(participant_bound["ok"], true, "{participant_bound}");

        let event = json!({
            "hook_event_name":"Stop",
            "session_id":format!("native-{platform}-session"),
            "hook_id":"reused-native-hook-id"
        });
        assert_eq!(
            run_stop_hook(&fixture, &binding, flag, &event),
            official_block
        );
        assert_eq!(
            run_stop_hook(&fixture, &binding, flag, &event),
            official_allow
        );

        let context = fixture.request(json!({
            "id":"context",
            "token":fixture.token,
            "op":"context",
            "run_id":run_id
        }));
        assert_eq!(
            context["context"]["activations"]["root"]["status"],
            "blocked"
        );
        driver.shutdown();
    }
}

#[test]
fn stop_response_precedes_exactly_once_activation_cleanup_and_parallel_work_continues() {
    let fixture = DriverFixture::new("participant-exit-cleanup");
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")));
    store.start_run_manifest("run-participant-cleanup").unwrap();
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env_values(
        "run-participant-cleanup",
        &[("HUMANIZE_TMUX_BIN", &fake_tmux_value)],
    );
    let bound = fixture.request(json!({
        "id":"bind",
        "token":fixture.token,
        "op":"bind_run",
        "run_id":"run-participant-cleanup",
        "flow_lock":reviewed_lock_package(),
        "tmux":{"enabled":true,"session":"host-a","window":"flow-a","agent_command":"humanize-test-agent"},
        "review":{"review_id":"review","status":"approved"}
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    let root_binding = participant_binding_for(&fixture, "run-participant-cleanup", "root");
    let delivered = fixture.request(json!({
        "id":"deliver",
        "token":fixture.token,
        "op":"deliver_artifact",
        "run_id":"run-participant-cleanup",
        "activation_id":"root",
        "artifact_id":"brief",
        "payload":"done"
    }));
    assert_eq!(delivered["ok"], true, "{delivered}");
    assert!(
        participant_binding_for_optional(&fixture, "run-participant-cleanup", "follow").is_some()
    );

    let stop = run_stop_hook(
        &fixture,
        &root_binding,
        "--codex-stop-hook",
        &json!({
            "hook_event_name":"Stop",
            "session_id":"fake-native-session",
            "hook_id":"root-stop"
        }),
    );
    assert_eq!(stop, json!({}));
    let before_exit = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert!(!before_exit.contains("kill-pane -t host-a:%7.%8"));

    let exit_started = Instant::now();
    let first_exit = run_participant_exit_hook(&fixture, &root_binding, 0);
    assert!(exit_started.elapsed() < Duration::from_millis(750));
    assert!(
        first_exit.status.success(),
        "{}",
        String::from_utf8_lossy(&first_exit.stderr)
    );
    assert!(first_exit.stdout.is_empty());
    assert!(first_exit.stderr.is_empty());
    wait_for(Duration::from_secs(2), || {
        fs::read_to_string(fixture.tmux_log())
            .is_ok_and(|log| log.contains("kill-pane -t host-a:%7.%8"))
    });
    wait_for(Duration::from_secs(2), || {
        let manifest = private_run_assets(&fixture, "run-participant-cleanup");
        manifest["activations"]["root"]["capture_complete"] == true
            && manifest["activations"]["root"]["resource_cleanup_status"] == "complete"
    });
    let after_exit = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert_eq!(after_exit.matches("kill-pane -t host-a:%7.%8").count(), 1);
    assert!(!after_exit.contains("kill-pane -t host-a:%7.%9"));
    let panes = fs::read_to_string(fixture.root.join("panes")).unwrap();
    assert!(!panes.contains("%8"));
    assert!(panes.contains("%9"));
    let manifest = private_run_assets(&fixture, "run-participant-cleanup");
    assert_eq!(manifest["activations"]["root"]["capture_complete"], true);
    assert_eq!(
        manifest["activations"]["root"]["resource_cleanup_status"],
        "complete"
    );

    let duplicate = run_participant_exit_hook(&fixture, &root_binding, 0);
    assert!(
        duplicate.status.success(),
        "{}",
        String::from_utf8_lossy(&duplicate.stderr)
    );
    let final_log = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert_eq!(final_log.matches("kill-pane -t host-a:%7.%8").count(), 1);
    assert_eq!(
        runtime_event_type_count(
            &fixture.run_events_path("run-participant-cleanup"),
            "participant_exited"
        ),
        1
    );
    let driver_events =
        fs::read_to_string(fixture.driver_events_path("run-participant-cleanup")).unwrap();
    assert!(!driver_events.contains("\"kind\":\"participant_exited\""));
    assert_eq!(
        public_event_kind_count(&fixture, "run-participant-cleanup", "agent_session.ended"),
        1
    );
    driver.shutdown();
}

#[test]
fn participant_exit_without_response_ack_replays_terminal_cleanup_once() {
    let fixture = DriverFixture::new("participant-exit-ack-crash");
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")));
    store
        .start_run_manifest("run-participant-exit-ack")
        .unwrap();
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let driver_env = [("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str())];
    let mut driver = fixture.spawn_with_env_values("run-participant-exit-ack", &driver_env);
    bind_and_activate_agent(&fixture, "run-participant-exit-ack");
    let binding = participant_binding(&fixture, "run-participant-exit-ack");

    let response = fixture.request(json!({
        "id":"participant-exit-no-ack",
        "token":fixture.token,
        "op":"participant_exited",
        "run_id":"run-participant-exit-ack",
        "activation_id":binding["activation_id"],
        "participant_handle":binding["handle"],
        "participant_credential":binding["credential"],
        "exit_status":17
    }));
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["response_ack_required"], true, "{response}");
    let status = fixture.request(json!({
        "id":"status-before-exit-ack",
        "token":fixture.token,
        "op":"status",
        "run_id":"run-participant-exit-ack"
    }));
    assert_eq!(status["ok"], true, "{status}");
    let context = fixture.request(json!({
        "id":"context-before-exit-ack",
        "token":fixture.token,
        "op":"context",
        "run_id":"run-participant-exit-ack"
    }));
    assert_eq!(context["ok"], true, "{context}");
    thread::sleep(Duration::from_millis(100));
    let before_crash = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert!(!before_crash.contains("kill-pane -t host-a:%7.%9"));

    driver.crash();
    assert_eq!(
        runtime_event_type_count(
            &fixture.run_events_path("run-participant-exit-ack"),
            "participant_exited"
        ),
        1
    );
    let driver_events =
        fs::read_to_string(fixture.driver_events_path("run-participant-exit-ack")).unwrap();
    assert!(!driver_events.contains("\"kind\":\"participant_exited\""));

    let mut restarted = fixture.spawn_with_env_values("run-participant-exit-ack", &driver_env);
    wait_for(Duration::from_secs(2), || {
        fs::read_to_string(fixture.tmux_log())
            .is_ok_and(|log| log.matches("kill-pane -t host-a:%7.%9").count() == 1)
    });
    let context = fixture.request(json!({
        "id":"context",
        "token":fixture.token,
        "op":"context",
        "run_id":"run-participant-exit-ack"
    }));
    assert_eq!(
        context["context"]["activations"]["manual"]["status"],
        "failed"
    );
    assert_eq!(
        context["context"]["activations"]["manual"]["participant"]["exited"],
        true
    );
    assert_eq!(
        context["context"]["activations"]["manual"]["participant"]["exit_status"],
        17
    );
    restarted.shutdown();
}

#[test]
fn participant_exit_idempotent_retry_without_ack_finishes_cleanup_once() {
    let fixture = DriverFixture::new("participant-exit-idempotent-retry");
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")));
    store
        .start_run_manifest("run-participant-exit-retry")
        .unwrap();
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let driver_env = [("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str())];
    let mut driver = fixture.spawn_with_env_values("run-participant-exit-retry", &driver_env);
    bind_and_activate_agent(&fixture, "run-participant-exit-retry");
    let binding = participant_binding(&fixture, "run-participant-exit-retry");
    let request = json!({
        "id":"participant-exit-retry",
        "token":fixture.token,
        "op":"participant_exited",
        "run_id":"run-participant-exit-retry",
        "activation_id":binding["activation_id"],
        "participant_handle":binding["handle"],
        "participant_credential":binding["credential"],
        "exit_status":19
    });

    let first = fixture.request(request.clone());
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["idempotent"], false, "{first}");
    thread::sleep(Duration::from_millis(100));
    assert!(
        !fs::read_to_string(fixture.tmux_log())
            .unwrap()
            .contains("kill-pane -t host-a:%7.%9")
    );

    let retry = fixture.request(request);
    assert_eq!(retry["ok"], true, "{retry}");
    assert_eq!(retry["idempotent"], true, "{retry}");
    wait_for(Duration::from_secs(2), || {
        fs::read_to_string(fixture.tmux_log())
            .is_ok_and(|log| log.matches("kill-pane -t host-a:%7.%9").count() == 1)
            && fs::read_to_string(fixture.driver_events_path("run-participant-exit-retry"))
                .is_ok_and(|events| events.matches("tmux_pane_cleanup_receipt").count() == 1)
    });
    assert_eq!(
        runtime_event_type_count(
            &fixture.run_events_path("run-participant-exit-retry"),
            "participant_exited"
        ),
        1
    );
    let driver_events =
        fs::read_to_string(fixture.driver_events_path("run-participant-exit-retry")).unwrap();
    assert_eq!(driver_events.matches("tmux_panes_released").count(), 1);
    assert_eq!(
        driver_events.matches("tmux_pane_cleanup_receipt").count(),
        1
    );
    driver.shutdown();
}

#[test]
fn participant_exit_online_cleanup_is_scoped_to_the_exact_binding() {
    for (completion, acknowledge_response) in [("ack", true), ("retry", false)] {
        let run_id = format!("run-participant-exit-scope-{completion}");
        let fixture = DriverFixture::new(&format!("participant-exit-scope-{completion}"));
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
            .start_run_manifest(&run_id)
            .unwrap();
        let fake_tmux = fixture.fake_tmux_with_agent_ready();
        let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
        let driver_env = [("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str())];
        let mut driver = fixture.spawn_with_env_values(&run_id, &driver_env);
        let bound = fixture.request(json!({
            "id":"bind",
            "token":fixture.token,
            "op":"bind_run",
            "run_id":run_id,
            "flow_lock":parallel_agent_flow(),
            "tmux":{"enabled":true,"session":"host-a","window":"flow-a","agent_command":"humanize-test-agent"},
            "review":{"review_id":"review","status":"approved"}
        }));
        assert_eq!(bound["ok"], true, "{bound}");
        let binding_a = participant_binding_for(&fixture, &run_id, "agent-a");
        let binding_b = participant_binding_for(&fixture, &run_id, "agent-b");
        let pane_a = binding_a["pane_id"].as_str().unwrap();
        let pane_b = binding_b["pane_id"].as_str().unwrap();
        assert_ne!(pane_a, pane_b);
        let kill_a = format!("kill-pane -t host-a:%7.{pane_a}");
        let kill_b = format!("kill-pane -t host-a:%7.{pane_b}");

        let exit_a = fixture.request(participant_exit_request(
            &binding_a,
            fixture.token,
            &run_id,
            "participant-exit-a",
            31,
        ));
        assert_eq!(exit_a["ok"], true, "{exit_a}");
        assert_eq!(exit_a["idempotent"], false, "{exit_a}");

        if acknowledge_response {
            let exit_b = run_participant_exit_hook(&fixture, &binding_b, 32);
            assert!(
                exit_b.status.success(),
                "{}",
                String::from_utf8_lossy(&exit_b.stderr)
            );
        } else {
            let request = participant_exit_request(
                &binding_b,
                fixture.token,
                &run_id,
                "participant-exit-b",
                32,
            );
            let first = fixture.request(request.clone());
            assert_eq!(first["ok"], true, "{first}");
            assert_eq!(first["idempotent"], false, "{first}");
            let retry = fixture.request(request);
            assert_eq!(retry["ok"], true, "{retry}");
            assert_eq!(retry["idempotent"], true, "{retry}");
        }

        wait_for(Duration::from_secs(2), || {
            fs::read_to_string(fixture.tmux_log())
                .is_ok_and(|log| log.matches(&kill_b).count() == 1)
                && fs::read_to_string(fixture.root.join("panes"))
                    .is_ok_and(|panes| !panes.contains(pane_b))
        });
        let online_log = fs::read_to_string(fixture.tmux_log()).unwrap();
        assert_eq!(online_log.matches(&kill_a).count(), 0, "{online_log}");
        assert_eq!(online_log.matches(&kill_b).count(), 1, "{online_log}");
        let online_panes = fs::read_to_string(fixture.root.join("panes")).unwrap();
        assert!(online_panes.contains(pane_a), "{online_panes}");
        assert!(!online_panes.contains(pane_b), "{online_panes}");

        driver.crash();
        let mut restarted = fixture.spawn_with_env_values(&run_id, &driver_env);
        wait_for(Duration::from_secs(2), || {
            fs::read_to_string(fixture.tmux_log())
                .is_ok_and(|log| log.matches(&kill_a).count() == 1)
                && fs::read_to_string(fixture.root.join("panes"))
                    .is_ok_and(|panes| !panes.contains(pane_a))
        });
        let recovered_log = fs::read_to_string(fixture.tmux_log()).unwrap();
        assert_eq!(recovered_log.matches(&kill_a).count(), 1, "{recovered_log}");
        assert_eq!(recovered_log.matches(&kill_b).count(), 1, "{recovered_log}");
        let recovered_panes = fs::read_to_string(fixture.root.join("panes")).unwrap();
        assert!(!recovered_panes.contains(pane_a), "{recovered_panes}");
        assert!(!recovered_panes.contains(pane_b), "{recovered_panes}");
        restarted.shutdown();
    }
}

#[test]
fn participant_exit_persistence_fault_keeps_live_and_replay_state_behind_commit() {
    let fixture = DriverFixture::new("participant-exit-persist-fault");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-participant-exit-fault")
        .unwrap();
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let marker = fixture.root.join("fail-runtime-append");
    let marker_value = marker.to_string_lossy().to_string();
    let driver_env = [
        ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
        ("HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_AT", "1"),
        (
            "HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_IF_EXISTS",
            marker_value.as_str(),
        ),
    ];
    let mut driver = fixture.spawn_with_env_values("run-participant-exit-fault", &driver_env);
    bind_and_activate_agent(&fixture, "run-participant-exit-fault");
    let binding = participant_binding(&fixture, "run-participant-exit-fault");
    fs::write(&marker, "fail").unwrap();

    let failed = run_participant_exit_hook(&fixture, &binding, 23);
    assert!(!failed.status.success());
    assert_eq!(
        runtime_event_type_count(
            &fixture.run_events_path("run-participant-exit-fault"),
            "participant_exited"
        ),
        0
    );
    let live = fixture.request(json!({
        "id":"context-live",
        "token":fixture.token,
        "op":"context",
        "run_id":"run-participant-exit-fault"
    }));
    assert_eq!(
        live["context"]["activations"]["manual"]["status"],
        "running"
    );
    assert_eq!(
        live["context"]["activations"]["manual"]["participant"]["exited"],
        false
    );
    assert!(
        !fs::read_to_string(fixture.tmux_log())
            .unwrap()
            .contains("kill-pane -t host-a:%7.%9")
    );

    driver.crash();
    fs::remove_file(&marker).unwrap();
    let mut restarted = fixture.spawn_with_env_values("run-participant-exit-fault", &driver_env);
    let replay = fixture.request(json!({
        "id":"context-replay",
        "token":fixture.token,
        "op":"context",
        "run_id":"run-participant-exit-fault"
    }));
    assert_eq!(
        replay["context"]["activations"]["manual"]["status"],
        "running"
    );
    assert_eq!(
        replay["context"]["activations"]["manual"]["participant"]["exited"],
        false
    );

    let succeeded = run_participant_exit_hook(&fixture, &binding, 23);
    assert!(
        succeeded.status.success(),
        "{}",
        String::from_utf8_lossy(&succeeded.stderr)
    );
    wait_for(Duration::from_secs(2), || {
        fs::read_to_string(fixture.tmux_log())
            .is_ok_and(|log| log.matches("kill-pane -t host-a:%7.%9").count() == 1)
    });
    restarted.shutdown();
}

#[test]
fn initial_participant_prompt_contains_only_task_outputs_resources_and_boundaries() {
    let fixture = DriverFixture::new("participant-prompt-contract");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-secret-internal")
        .unwrap();
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let mut driver =
        fixture.spawn_with_env("run-secret-internal", &[("HUMANIZE_TMUX_BIN", &fake_tmux)]);
    let bound = fixture.request(json!({
        "id":"bind",
        "token":fixture.token,
        "op":"bind_run",
        "run_id":"run-secret-internal",
        "flow_lock":reviewed_lock_package(),
        "tmux":{"enabled":true,"session":"host-a","window":"flow-a","agent_command":"humanize-test-agent"}
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    wait_for(Duration::from_secs(2), || {
        fs::read_to_string(fixture.tmux_log()).is_ok_and(|log| {
            log.contains("Create the brief.")
                && log.matches("send-keys -t host-a:%7.%8 Enter").count() >= 2
        })
    });
    let log = fs::read_to_string(fixture.tmux_log()).unwrap();
    let prompt_start = log.find(" -- Create the brief.").unwrap() + 4;
    let prompt_tail = &log[prompt_start..];
    let prompt_end = prompt_tail.find("\ndisplay-message -p -t ").unwrap();
    let prompt = &prompt_tail[..prompt_end];
    for required in [
        "Create the brief.",
        "Exact reviewed lock package fixture.",
        "Do only the reviewed work.",
        "brief",
        "read_only",
        "restricted",
        "Humanize",
        "normally",
    ] {
        assert!(prompt.contains(required), "missing {required}: {prompt}");
    }
    for forbidden in [
        "run-secret-internal",
        "README.md",
        "rule.safety",
        "prompt.root",
        "schema.brief",
        "flow_lock",
        "route",
        "pane",
        "driver",
        "experiment",
        "retry",
    ] {
        assert!(!prompt.contains(forbidden), "leaked {forbidden}: {prompt}");
    }
    driver.shutdown();
}

#[test]
fn participant_exit_without_stop_marks_failed_and_uses_terminal_cleanup() {
    let fixture = DriverFixture::new("participant-failed-cleanup");
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")));
    store.start_run_manifest("run-participant-failed").unwrap();
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let mut driver = fixture.spawn_with_env(
        "run-participant-failed",
        &[("HUMANIZE_TMUX_BIN", &fake_tmux)],
    );
    bind_and_activate_agent(&fixture, "run-participant-failed");
    let binding = participant_binding(&fixture, "run-participant-failed");
    wait_for(Duration::from_secs(2), || {
        fs::read_to_string(fixture.driver_events_path("run-participant-failed"))
            .is_ok_and(|events| events.contains("\"kind\":\"participant_bound\""))
    });

    let exited = run_participant_exit_hook(&fixture, &binding, 17);
    assert!(
        exited.status.success(),
        "{}",
        String::from_utf8_lossy(&exited.stderr)
    );
    wait_for(Duration::from_secs(2), || {
        private_run_assets(&fixture, "run-participant-failed")["activations"]["manual"]["resource_cleanup_status"]
            == "complete"
    });
    let context = fixture.request(json!({
        "id":"context",
        "token":fixture.token,
        "op":"context",
        "run_id":"run-participant-failed"
    }));
    assert_eq!(
        context["context"]["activations"]["manual"]["status"],
        "failed"
    );
    let tmux = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert_eq!(tmux.matches("kill-pane -t host-a:%7.%9").count(), 1);
    driver.shutdown();
}

pub(super) fn bind_and_activate_agent(fixture: &DriverFixture, run_id: &str) {
    let bound = fixture.request(json!({
        "id": "bind",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": run_id,
        "flow_lock": later_agent_flow(),
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        },
        "review": {"review_id":"review", "status":"approved"}
    }));
    assert_eq!(bound["ok"], true, "{bound}");
    let activated = fixture.request(json!({
        "id": "activate",
        "token": fixture.token,
        "op": "activate",
        "run_id": run_id,
        "node_id": "manual"
    }));
    assert_eq!(activated["ok"], true, "{activated}");
}

pub(super) fn participant_binding(fixture: &DriverFixture, run_id: &str) -> Value {
    participant_binding_for(fixture, run_id, "manual")
}

fn participant_binding_for(fixture: &DriverFixture, run_id: &str, activation_id: &str) -> Value {
    participant_binding_for_optional(fixture, run_id, activation_id).unwrap()
}

fn participant_binding_for_optional(
    fixture: &DriverFixture,
    run_id: &str,
    activation_id: &str,
) -> Option<Value> {
    let mut binding = fs::read_to_string(fixture.driver_events_path(run_id))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| {
            event["kind"] == "participant_started"
                && event["payload"]["binding"]["activation_id"] == activation_id
        })
        .map(|event| event["payload"]["binding"].clone())?;
    let path = fixture.private_run_root(run_id).join(
        binding["binding_file"]
            .as_str()
            .expect("participant binding event must reference its private file"),
    );
    let private_binding: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    binding["credential"] = private_binding["credential"].clone();
    binding["binding_file_path"] = Value::String(path.to_string_lossy().into_owned());
    Some(binding)
}

fn private_run_assets(fixture: &DriverFixture, run_id: &str) -> Value {
    serde_json::from_slice(
        &fs::read(fixture.private_driver_dir(run_id).join("run-assets.json")).unwrap(),
    )
    .unwrap()
}

pub(super) fn participant_bind_request(binding: &Value, token: &str, run_id: &str) -> Value {
    participant_bind_request_for_platform(binding, token, run_id, "codex", "codex_session_start")
}

fn participant_exit_request(
    binding: &Value,
    token: &str,
    run_id: &str,
    id: &str,
    exit_status: i32,
) -> Value {
    json!({
        "id":id,
        "token":token,
        "op":"participant_exited",
        "run_id":run_id,
        "activation_id":binding["activation_id"],
        "participant_handle":binding["handle"],
        "participant_credential":binding["credential"],
        "exit_status":exit_status
    })
}

fn participant_bind_request_for_platform(
    binding: &Value,
    token: &str,
    run_id: &str,
    platform: &str,
    source: &str,
) -> Value {
    json!({
        "id":"participant-bind",
        "token":token,
        "op":"participant_bind",
        "run_id":run_id,
        "activation_id":binding["activation_id"],
        "allocation_generation":binding["allocation_generation"],
        "pane_id":binding["pane_id"],
        "readiness_nonce":binding["readiness_nonce"],
        "participant_handle":binding["handle"],
        "participant_credential":binding["credential"],
        "native_session_id":format!("native-{platform}-session"),
        "platform":platform,
        "source":source
    })
}

fn run_stop_hook(_fixture: &DriverFixture, binding: &Value, flag: &str, event: &Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .arg(flag)
        .env(
            "HUMANIZE_PARTICIPANT_BINDING_FILE",
            binding["binding_file_path"].as_str().unwrap(),
        )
        .env("TMUX_PANE", binding["pane_id"].as_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.as_mut().unwrap(), "{event}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn run_participant_exit_hook(
    _fixture: &DriverFixture,
    binding: &Value,
    exit_status: i32,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .arg("--participant-exited-hook")
        .arg("--exit-status")
        .arg(exit_status.to_string())
        .env(
            "HUMANIZE_PARTICIPANT_BINDING_FILE",
            binding["binding_file_path"].as_str().unwrap(),
        )
        .output()
        .unwrap()
}

fn wait_for(timeout: Duration, predicate: impl Fn() -> bool) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("condition was not satisfied within {timeout:?}");
}

fn assert_tree_does_not_contain(root: &Path, secret: &str) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_tree_does_not_contain(&path, secret);
        } else if path.is_file() {
            let bytes = fs::read(&path).unwrap();
            assert!(
                !bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "secret leaked into {}",
                path.display()
            );
        }
    }
}

fn runtime_event_type_count(path: &Path, expected: &str) -> usize {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .flat_map(|batch| batch["events"].as_array().cloned().unwrap_or_default())
        .filter(|event| event["payload"]["type"] == expected)
        .count()
}

fn public_event_kind_count(fixture: &DriverFixture, run_id: &str, expected: &str) -> usize {
    fs::read_to_string(fixture.run_root(run_id).join("records/events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["kind"] == expected)
        .inspect(|event| {
            assert!(
                event["session_ref"]
                    .as_str()
                    .is_some_and(|reference| !reference.is_empty()),
                "{event}"
            );
        })
        .count()
}

fn participant_stop_request(
    binding: &Value,
    token: &str,
    run_id: &str,
    invocation_id: &str,
) -> Value {
    json!({
        "id":invocation_id,
        "token":token,
        "op":"participant_stop",
        "run_id":run_id,
        "activation_id":binding["activation_id"],
        "participant_handle":binding["handle"],
        "participant_credential":binding["credential"],
        "native_session_id":"native-codex-session",
        "invocation_id":invocation_id,
        "reason":"participant requested stop"
    })
}

fn participant_mcp_requests(
    _fixture: &DriverFixture,
    binding: &Value,
    requests: &[Value],
    credential_override: Option<&str>,
) -> Vec<Value> {
    let original_binding_path = Path::new(binding["binding_file_path"].as_str().unwrap());
    let override_path = credential_override.map(|credential| {
        let mut private_binding: Value =
            serde_json::from_slice(&fs::read(original_binding_path).unwrap()).unwrap();
        private_binding["credential"] = Value::String(credential.to_string());
        let path = original_binding_path
            .parent()
            .unwrap()
            .join("participant-binding-override.json");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        serde_json::to_writer(&mut file, &private_binding).unwrap();
        file.sync_all().unwrap();
        path
    });
    let binding_path = override_path.as_deref().unwrap_or(original_binding_path);
    let mut child = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .env("HUMANIZE_PARTICIPANT_BINDING_FILE", binding_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    for request in requests {
        writeln!(child.stdin.as_mut().unwrap(), "{request}").unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if let Some(path) = override_path {
        fs::remove_file(path).unwrap();
    }
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
