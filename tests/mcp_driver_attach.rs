mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::adapters::tmux::SystemCommandRunner;
use humanize_plugin::driver::{DriverClient, acquire_driver_attach_lock};
use humanize_plugin::mcp::{McpServer, TmuxExecutionDefaults};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use support::driver_tmux::ControlledTmuxFixture;
use support::mcp::{call_tool, lock_flow, structured};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn attach_lock_rejects_symlinked_private_run_ancestor_without_external_creation() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let root = std::env::temp_dir()
        .join("humanize-plugin-driver-attach")
        .join(format!("attach-lock-symlink-{}", std::process::id()));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    let run_root = root.join("runs/run-attach-lock-symlink");
    let runtime_root = root.join("runtime");
    let outside = root.join("outside");
    fs::create_dir_all(&run_root).unwrap();
    fs::create_dir_all(&runtime_root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    for path in [&runtime_root, &outside] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let identity = std::path::absolute(&run_root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    symlink(
        &outside,
        runtime_root.join(format!("r{:016x}", stable_hash(&identity))),
    )
    .unwrap();
    let prior_state_root = std::env::var_os("HUMANIZE_STATE_ROOT");
    unsafe {
        std::env::set_var("HUMANIZE_STATE_ROOT", &root);
    }

    let result = acquire_driver_attach_lock(&run_root);

    if let Some(value) = prior_state_root {
        unsafe { std::env::set_var("HUMANIZE_STATE_ROOT", value) };
    } else {
        unsafe { std::env::remove_var("HUMANIZE_STATE_ROOT") };
    }
    assert!(result.is_err(), "symlinked private run root was accepted");
    assert!(
        !outside.join("driver/attach.lock").exists(),
        "attach lock escaped into the symlink target"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn crashed_driver_is_replaced_before_resume_without_duplicate_node_input() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let fixture = AttachFixture::new("crash-resume");
    let mut server = fixture.server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-crash-resume",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert_eq!(structured(&started)["ok"], true, "{started}");
    fixture.wait_for_helpers();
    fixture.kill_current_driver();

    let mut restarted_mcp = fixture.server();
    let resumed = call_tool(
        &mut restarted_mcp,
        3,
        "resume_run",
        json!({ "run_id": "run-crash-resume" }),
    );

    assert_eq!(structured(&resumed)["ok"], true, "{resumed}");
    assert_eq!(structured(&resumed)["run_status"], "running");
    fixture.wait_for_helpers();
    let log = wait_for_text(&fixture.root.join("tmux.log"), "driver-launch-count=2");
    assert_eq!(log.matches("driver-launch-count=").count(), 2, "{log}");
    assert_eq!(log.matches("agent-launch-input").count(), 1, "{log}");
    assert_eq!(log.matches("node-prompt-enter").count(), 1, "{log}");
    fixture.shutdown_driver("run-crash-resume");
}

#[test]
fn public_manifest_attacks_do_not_change_private_recovery_or_pane_cleanup() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    for attack in ["missing", "corrupt", "retagged"] {
        let fixture = AttachFixture::new(&format!("public-manifest-{attack}"));
        let run_id = format!("run-public-manifest-{attack}");
        let mut server = fixture.server();
        let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
        let started = call_tool(
            &mut server,
            2,
            "run_flow",
            json!({
                "run_id": run_id,
                "flow_lock_id": lock_id,
                "content_hash": content_hash,
            }),
        );
        assert_eq!(structured(&started)["ok"], true, "{attack}: {started}");
        fixture.wait_for_helpers();
        fixture.kill_current_driver();

        let run_root = fixture.run_root(&run_id);
        let public_manifest = run_root.join("manifest.json");
        match attack {
            "missing" => fs::remove_file(&public_manifest).unwrap(),
            "corrupt" => fs::write(&public_manifest, b"{not-json\n").unwrap(),
            "retagged" => fs::write(
                &public_manifest,
                serde_json::to_vec_pretty(&json!({
                    "schema_name": "humanize.public_manifest",
                    "schema_major": 1,
                    "run_ref": "sha256:forged-run",
                    "status": "completed",
                    "activations": {
                        "sha256:forged-activation": {
                            "state": "closed",
                            "resource_cleanup_status": "complete"
                        }
                    },
                    "journal": {
                        "status": "sealed",
                        "event_count": 999,
                        "last_seq": 999,
                        "current_sha256": "sha256:forged",
                        "final_sha256": "sha256:forged"
                    }
                }))
                .unwrap(),
            )
            .unwrap(),
            _ => unreachable!(),
        }

        let mut restarted_mcp = fixture.server();
        let stopped = call_tool(
            &mut restarted_mcp,
            3,
            "stop_run",
            json!({ "run_id": run_id }),
        );
        assert_eq!(structured(&stopped)["ok"], true, "{attack}: {stopped}");
        let log = wait_for_text(&fixture.root.join("tmux.log"), "kill-pane -t host-a:%7.%9");
        assert_eq!(
            log.lines()
                .filter(|line| *line == "kill-pane -t host-a:%7.%9")
                .count(),
            1,
            "{attack}: {log}"
        );
        let private_manifest: Value = serde_json::from_slice(
            &fs::read(private_driver_dir(&run_root).join("run-assets.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            private_manifest["activations"]["root"]["resource_cleanup_status"], "complete",
            "{attack}"
        );
        let repaired: Value = serde_json::from_slice(&fs::read(&public_manifest).unwrap()).unwrap();
        assert_eq!(repaired["schema_name"], "humanize.public_manifest");
        assert_ne!(repaired["run_ref"], "sha256:forged-run");
        fixture.shutdown_driver(&run_id);
    }
}

#[test]
fn recovery_errors_do_not_expose_private_paths_or_revision_identifiers() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let fixture = AttachFixture::new("recovery-error-sanitization");
    let run_id = "run-recovery-error-sanitization";
    let mut server = fixture.server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": run_id,
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert_eq!(structured(&started)["ok"], true, "{started}");
    fixture.wait_for_helpers();
    fixture.kill_current_driver();

    let private_driver = private_driver_dir(&fixture.run_root(run_id));
    let revision = fs::read_dir(private_driver.join("revisions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let revision_name = revision.file_name().unwrap().to_string_lossy().into_owned();
    fs::remove_file(&revision).unwrap();

    let mut restarted_mcp = fixture.server();
    let response = call_tool(
        &mut restarted_mcp,
        3,
        "run_status",
        json!({ "run_id": run_id }),
    );
    let response_bytes = serde_json::to_vec(&response).unwrap();
    let private_bytes = private_driver.to_string_lossy();

    assert_eq!(structured(&response)["ok"], false, "{response}");
    assert_eq!(
        structured(&response)["error"]["code"],
        "driver_unavailable",
        "{response}"
    );
    assert!(
        !response_bytes
            .windows(private_bytes.len())
            .any(|window| window == private_bytes.as_bytes()),
        "private driver path leaked through the MCP response: {response}"
    );
    assert!(
        !response_bytes
            .windows(revision_name.len())
            .any(|window| window == revision_name.as_bytes()),
        "private revision identifier leaked through the MCP response: {response}"
    );
    let diagnostics = fs::read_to_string(private_driver.join("mcp-diagnostics.jsonl")).unwrap();
    assert!(
        diagnostics.contains(private_bytes.as_ref()),
        "{diagnostics}"
    );
    assert!(diagnostics.contains(&revision_name), "{diagnostics}");
}

#[test]
fn graceful_driver_shutdown_is_replaced_before_resume() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let fixture = AttachFixture::new("graceful-resume");
    let mut server = fixture.server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-graceful-resume",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert_eq!(structured(&started)["ok"], true, "{started}");
    fixture.wait_for_helpers();
    fixture.graceful_shutdown("run-graceful-resume");
    let finalized_manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(
            private_driver_dir(&fixture.run_root("run-graceful-resume")).join("run-assets.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        finalized_manifest["activations"]["root"]["capture_phase"],
        "complete"
    );
    assert_eq!(
        finalized_manifest["activations"]["root"]["resource_cleanup_status"],
        "complete"
    );

    let mut restarted_mcp = fixture.server();
    let attached = call_tool(
        &mut restarted_mcp,
        3,
        "run_flow",
        json!({ "run_id": "run-graceful-resume" }),
    );
    assert_eq!(structured(&attached)["ok"], true, "{attached}");
    assert_eq!(structured(&attached)["attached"], true);
    assert_eq!(structured(&attached)["run_status"], "paused", "{attached}");
    let suspended = call_tool(
        &mut restarted_mcp,
        4,
        "run_status",
        json!({ "run_id": "run-graceful-resume" }),
    );
    assert_eq!(
        structured(&suspended)["context"]["activations"]["root"]["status"],
        "running",
        "{suspended}"
    );
    let barriers = structured(&suspended)["context"]["ambiguous_deliveries"]
        .as_array()
        .unwrap()
        .clone();
    let mut resumed = Value::Null;
    for (index, barrier) in barriers.iter().enumerate() {
        let role = barrier["role"].as_str().unwrap();
        resumed = call_tool(
            &mut restarted_mcp,
            5 + index as u64,
            "resume_run",
            json!({
                "run_id": "run-graceful-resume",
                "delivery_resolution": {
                    "started_event_sequence": barrier["started_event_sequence"],
                    "outcome": if role == "node_prompt" { "not_submitted" } else { "submitted" },
                    "evidence": "operator confirmed the released pane cannot continue the prior work"
                }
            }),
        );
    }

    assert_eq!(structured(&resumed)["ok"], true, "{resumed}");
    assert_eq!(structured(&resumed)["run_status"], "running");
    fixture.wait_for_helpers();
    assert_eq!(
        structured(&resumed)["actuation"]["warnings"][0]["status"],
        "readiness_pending",
        "{resumed}"
    );
    assert_eq!(
        structured(&resumed)["tmux_allocations"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "{resumed}"
    );
    assert_eq!(
        structured(&resumed)["tmux_allocations"][0]["allocation_generation"],
        1,
        "{resumed}"
    );
    let log = wait_for_text(&fixture.root.join("tmux.log"), "driver-launch-count=2");
    assert_eq!(log.matches("driver-launch-count=").count(), 2, "{log}");
    assert_eq!(log.matches("agent-launch-input").count(), 2, "{log}");
    assert_eq!(log.matches("node-prompt-enter").count(), 2, "{log}");
    fixture.shutdown_driver("run-graceful-resume");
}

#[test]
fn graceful_shutdown_holds_attach_lock_before_listener_stops() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let fixture = AttachFixture::new("graceful-lock-boundary");
    let mut server = fixture.server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-graceful-lock",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert_eq!(structured(&started)["ok"], true, "{started}");
    fixture.wait_for_helpers();

    let run_root = fixture.run_root("run-graceful-lock");
    let attach_lock = acquire_driver_attach_lock(&run_root).unwrap();
    let client = DriverClient::from_run_root_for_run(&run_root, "run-graceful-lock")
        .unwrap()
        .unwrap();
    let shutdown = client
        .request("shutdown", "run-graceful-lock", &json!({}))
        .unwrap();
    assert_eq!(shutdown["ok"], true, "{shutdown}");
    thread::sleep(Duration::from_millis(150));

    let pid = fs::read_to_string(fixture.root.join("driver.pid"))
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    assert!(process_exists(pid));
    let status = client
        .request("status", "run-graceful-lock", &json!({}))
        .expect("listener must remain available until finalization owns attach lock");
    assert_eq!(status["ok"], true, "{status}");

    drop(attach_lock);
    let wait_started = Instant::now();
    while private_driver_dir(&run_root).join("ipc.json").exists()
        && wait_started.elapsed() < Duration::from_secs(5)
    {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!private_driver_dir(&run_root).join("ipc.json").exists());

    let mut restarted_mcp = fixture.server();
    let attached_status = call_tool(
        &mut restarted_mcp,
        3,
        "run_status",
        json!({ "run_id": "run-graceful-lock" }),
    );
    let barriers = structured(&attached_status)["context"]["ambiguous_deliveries"]
        .as_array()
        .unwrap()
        .clone();
    let mut resumed = Value::Null;
    for (index, barrier) in barriers.iter().enumerate() {
        let role = barrier["role"].as_str().unwrap();
        resumed = call_tool(
            &mut restarted_mcp,
            4 + index as u64,
            "resume_run",
            json!({
                "run_id": "run-graceful-lock",
                "delivery_resolution": {
                    "started_event_sequence": barrier["started_event_sequence"],
                    "outcome": if role == "node_prompt" { "not_submitted" } else { "submitted" },
                    "evidence": "operator confirmed the finalized pane cannot continue prior work"
                }
            }),
        );
    }
    assert_eq!(structured(&resumed)["ok"], true, "{resumed}");
    fixture.wait_for_helpers();
    assert_eq!(
        structured(&resumed)["tmux_allocations"][0]["allocation_generation"],
        1,
        "{resumed}"
    );
    let log = wait_for_text(&fixture.root.join("tmux.log"), "driver-launch-count=2");
    assert_eq!(log.matches("driver-launch-count=").count(), 2, "{log}");
    assert_eq!(log.matches("agent-launch-input").count(), 2, "{log}");
    assert_eq!(log.matches("node-prompt-enter").count(), 2, "{log}");
    fixture.shutdown_driver("run-graceful-lock");
}

#[test]
fn live_attach_probe_does_not_repair_an_in_progress_driver_event_tail() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let fixture = AttachFixture::new("live-append-race");
    let run_id = "run-live-append-race";
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")));
    let manifest = store.start_run_manifest(run_id).unwrap();
    let run_root = manifest.root;
    let (events_path, listener, socket_path) =
        install_live_probe_endpoint(&fixture.root, &run_root, run_id);
    let durable = b"{\"seq\":1,\"at_ms\":1,\"kind\":\"driver_pane_owned\",\"payload\":{}}\n";
    fs::write(&events_path, durable).unwrap();
    fs::set_permissions(&events_path, fs::Permissions::from_mode(0o600)).unwrap();
    let partial = br#"{"seq":999999,"kind":"concurrent""#;
    let mut writer = fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .unwrap();
    writer.write_all(partial).unwrap();
    writer.sync_all().unwrap();

    let endpoint = thread::spawn(move || serve_live_probe_endpoint(listener, run_id));

    let mut restarted_mcp = fixture.server();
    let status = call_tool(
        &mut restarted_mcp,
        3,
        "run_status",
        json!({ "run_id": run_id }),
    );
    let _ = UnixStream::connect(&socket_path);
    let requests = endpoint.join().unwrap();

    assert_eq!(structured(&status)["ok"], true, "{status}");
    let bytes = fs::read(&events_path).unwrap();
    assert_eq!(bytes.len(), durable.len() + partial.len());
    assert!(bytes.ends_with(partial));
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(requests.iter().all(|request| {
        request["op"] == "status"
            && request["run_id"] == run_id
            && request["token"] == "live-probe-token"
    }));

    writer.write_all(b",\"at_ms\":2,\"payload\":{}}\n").unwrap();
    writer.sync_all().unwrap();
    drop(writer);
    for line in fs::read_to_string(&events_path).unwrap().lines() {
        serde_json::from_str::<Value>(line).unwrap();
    }
}

#[test]
fn run_flow_retry_attaches_before_local_lock_or_manifest_work() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let fixture = AttachFixture::new("run-flow-retry");
    let mut server = fixture.server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-flow-retry",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert_eq!(structured(&started)["ok"], true, "{started}");
    fixture.wait_for_helpers();
    let manifest_before =
        fs::read(fixture.run_root("run-flow-retry").join("manifest.json")).unwrap();

    let mut restarted_mcp = fixture.server();
    let retried = call_tool(
        &mut restarted_mcp,
        3,
        "run_flow",
        json!({ "run_id": "run-flow-retry" }),
    );

    assert_eq!(structured(&retried)["ok"], true, "{retried}");
    assert_eq!(structured(&retried)["attached"], true);
    assert_eq!(
        fs::read(fixture.run_root("run-flow-retry").join("manifest.json")).unwrap(),
        manifest_before
    );
    let log = fs::read_to_string(fixture.root.join("tmux.log")).unwrap();
    assert_eq!(log.matches("driver-launch-count=").count(), 1, "{log}");
    fixture.shutdown_driver("run-flow-retry");
}

#[test]
fn concurrent_mcp_attachers_converge_on_one_replacement_driver() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let fixture = AttachFixture::new("concurrent-attach");
    let mut server = fixture.server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-concurrent-attach",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert_eq!(structured(&started)["ok"], true, "{started}");
    fixture.wait_for_helpers();
    fixture.kill_current_driver();

    let handles = (0..2)
        .map(|index| {
            let root = fixture.root.clone();
            thread::spawn(move || {
                let mut server = server_for_root(&root);
                call_tool(
                    &mut server,
                    10 + index,
                    "run_status",
                    json!({ "run_id": "run-concurrent-attach" }),
                )
            })
        })
        .collect::<Vec<_>>();
    let responses = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    for response in responses {
        assert_eq!(structured(&response)["ok"], true, "{response}");
    }
    let log = wait_for_text(&fixture.root.join("tmux.log"), "driver-launch-count=2");
    assert_eq!(log.matches("driver-launch-count=").count(), 2, "{log}");
    fixture.shutdown_driver("run-concurrent-attach");
}

#[test]
fn stale_token_socket_and_operator_pane_are_reconciled_without_touching_node_input() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let fixture = AttachFixture::new("stale-artifacts");
    let mut server = fixture.server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-stale-artifacts",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert_eq!(structured(&started)["ok"], true, "{started}");
    fixture.wait_for_helpers();
    fixture.kill_current_driver();
    let token_path = private_driver_dir(&fixture.run_root("run-stale-artifacts")).join("ipc-token");
    fs::write(&token_path, "stale-token\n").unwrap();
    let mut permissions = fs::metadata(&token_path).unwrap().permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&token_path, permissions).unwrap();

    let mut restarted_mcp = fixture.server();
    let status = call_tool(
        &mut restarted_mcp,
        3,
        "run_status",
        json!({ "run_id": "run-stale-artifacts" }),
    );

    assert_eq!(structured(&status)["ok"], true, "{status}");
    let token = fs::read_to_string(&token_path).unwrap();
    assert_ne!(token.trim(), "stale-token");
    assert_eq!(token.trim().len(), 64);
    let killed = fs::read_to_string(fixture.root.join("killed-panes")).unwrap();
    assert_eq!(killed.matches("host-a:%7.%8").count(), 1, "{killed}");
    assert!(!killed.contains("host-a:%7.%9"), "{killed}");
    let log = fs::read_to_string(fixture.root.join("tmux.log")).unwrap();
    assert_eq!(log.matches("agent-launch-input").count(), 1, "{log}");
    assert_eq!(log.matches("node-prompt-enter").count(), 1, "{log}");
    fixture.shutdown_driver("run-stale-artifacts");
}

#[test]
fn attach_reconciles_absent_replayed_node_pane_before_status() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let fixture = AttachFixture::new("stale-node-pane");
    let mut server = fixture.server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-stale-node-pane",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert_eq!(structured(&started)["ok"], true, "{started}");
    fixture.wait_for_helpers();
    fixture.kill_current_driver();
    fs::write(fixture.root.join("killed-panes"), "host-a:%7.%9\n").unwrap();
    fs::write(fixture.root.join("pipe-9.stop"), "stopped\n").unwrap();
    thread::sleep(Duration::from_millis(100));

    let mut restarted_mcp = fixture.server();
    let status = call_tool(
        &mut restarted_mcp,
        3,
        "run_status",
        json!({ "run_id": "run-stale-node-pane" }),
    );

    assert_eq!(structured(&status)["ok"], true, "{status}");
    fixture.wait_for_helpers();
    let events = fs::read_to_string(
        private_driver_dir(&fixture.run_root("run-stale-node-pane")).join("driver-events.jsonl"),
    )
    .unwrap();
    assert!(
        events.contains("\"kind\":\"tmux_panes_released\""),
        "{events}"
    );
    let barriers = structured(&status)["context"]["ambiguous_deliveries"]
        .as_array()
        .unwrap()
        .clone();
    let mut resumed = Value::Null;
    for (index, barrier) in barriers.iter().enumerate() {
        let role = barrier["role"].as_str().unwrap();
        resumed = call_tool(
            &mut restarted_mcp,
            4 + index as u64,
            "resume_run",
            json!({
                "run_id": "run-stale-node-pane",
                "delivery_resolution": {
                    "started_event_sequence": barrier["started_event_sequence"],
                    "outcome": if role == "node_prompt" { "not_submitted" } else { "submitted" },
                    "evidence": "the prior pane is confirmed absent and cannot retain active work"
                }
            }),
        );
    }
    assert_eq!(structured(&resumed)["ok"], true, "{resumed}");
    fixture.wait_for_helpers();
    assert_eq!(
        structured(&resumed)["tmux_allocations"][0]["allocation_generation"],
        1,
        "{resumed}"
    );
    let log = fs::read_to_string(fixture.root.join("tmux.log")).unwrap();
    assert_eq!(log.matches("agent-launch-input").count(), 2, "{log}");
    assert_eq!(log.matches("node-prompt-enter").count(), 2, "{log}");
    fixture.shutdown_driver("run-stale-node-pane");
}

struct AttachFixture {
    root: PathBuf,
    prior_tmux: Option<std::ffi::OsString>,
    prior_state_root: Option<std::ffi::OsString>,
    tmux_control: ControlledTmuxFixture,
}

impl AttachFixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .join("humanize-plugin-driver-attach")
            .join(format!("{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        let tmux_control = ControlledTmuxFixture::new(&root);
        let fake_tmux = fake_tmux(&root);
        let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
        let prior_state_root = std::env::var_os("HUMANIZE_STATE_ROOT");
        unsafe {
            std::env::set_var("HUMANIZE_TMUX_BIN", fake_tmux);
            std::env::set_var("HUMANIZE_STATE_ROOT", &root);
        }
        Self {
            root,
            prior_tmux,
            prior_state_root,
            tmux_control,
        }
    }

    fn server(&self) -> McpServer<SystemCommandRunner> {
        server_for_root(&self.root)
    }

    fn run_root(&self, run_id: &str) -> PathBuf {
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(self.root.join("runs")))
            .run_root(run_id)
            .unwrap()
    }

    fn kill_current_driver(&self) {
        self.wait_for_helpers();
        let pid = wait_for_text(&self.root.join("driver.pid"), "")
            .trim()
            .parse::<i32>()
            .unwrap();
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let started = Instant::now();
        while process_exists(pid) && started.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn graceful_shutdown(&self, run_id: &str) {
        self.wait_for_helpers();
        let run_root = self.run_root(run_id);
        let client = DriverClient::from_run_root_for_run(&run_root, run_id)
            .unwrap()
            .unwrap();
        let response = client.request("shutdown", run_id, &json!({})).unwrap();
        assert_eq!(response["ok"], true, "{response}");
        let started = Instant::now();
        while private_driver_dir(&run_root).join("ipc.json").exists()
            && started.elapsed() < Duration::from_secs(5)
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!private_driver_dir(&run_root).join("ipc.json").exists());
    }

    fn shutdown_driver(&self, run_id: &str) {
        self.wait_for_helpers();
        let run_root = self.run_root(run_id);
        if let Ok(Some(client)) = DriverClient::from_run_root_for_run(&run_root, run_id) {
            let _ = client.request("shutdown", run_id, &json!({}));
        }
        let started = Instant::now();
        while private_driver_dir(&run_root).join("ipc.json").exists()
            && started.elapsed() < Duration::from_secs(5)
        {
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_helpers(&self) {
        assert!(
            self.tmux_control.wait_for_hooks(),
            "fake SessionStart helper did not exit"
        );
    }
}

fn server_for_root(root: &Path) -> McpServer<SystemCommandRunner> {
    McpServer::with_tmux_runner_run_asset_store_and_execution_defaults(
        SystemCommandRunner,
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs"))),
        TmuxExecutionDefaults {
            session: Some("host-a".into()),
            window: Some("flow-a".into()),
            agent_command: Some("humanize-test-agent".into()),
        },
    )
}

fn private_driver_dir(run_root: &Path) -> PathBuf {
    let runs_root = run_root.parent().unwrap();
    let runtime_root = runs_root.parent().unwrap_or(runs_root).join("runtime");
    let identity = std::path::absolute(run_root)
        .unwrap_or_else(|_| run_root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    runtime_root
        .join(format!("r{:016x}", stable_hash(&identity)))
        .join("driver")
}

fn install_live_probe_endpoint(
    state_root: &Path,
    run_root: &Path,
    run_id: &str,
) -> (PathBuf, UnixListener, PathBuf) {
    let runtime_root = state_root.join("runtime");
    fs::create_dir_all(&runtime_root).unwrap();
    fs::set_permissions(&runtime_root, fs::Permissions::from_mode(0o700)).unwrap();
    let driver_dir = private_driver_dir(run_root);
    let private_run_root = driver_dir.parent().unwrap();
    fs::create_dir_all(&driver_dir).unwrap();
    fs::set_permissions(private_run_root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&driver_dir, fs::Permissions::from_mode(0o700)).unwrap();

    let identity_path = private_run_root.join("identity.json");
    fs::write(
        &identity_path,
        serde_json::to_vec_pretty(&json!({
            "schema": "humanize.private_run_identity.v1",
            "run_id": run_id,
            "public_run_root": std::path::absolute(run_root).unwrap(),
            "runs_root": std::path::absolute(run_root.parent().unwrap()).unwrap()
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&identity_path, fs::Permissions::from_mode(0o600)).unwrap();

    let token_path = driver_dir.join("ipc-token");
    fs::write(&token_path, b"live-probe-token\n").unwrap();
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).unwrap();
    let metadata_path = driver_dir.join("ipc.json");
    fs::write(
        &metadata_path,
        serde_json::to_vec_pretty(&json!({
            "run_id": run_id,
            "socket_path": "s",
            "auth_token_path": "ipc-token",
            "updated_at_ms": 1
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&metadata_path, fs::Permissions::from_mode(0o600)).unwrap();

    let socket_path = private_run_root.join("s");
    let listener = UnixListener::bind(&socket_path).unwrap();
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).unwrap();
    (
        driver_dir.join("driver-events.jsonl"),
        listener,
        socket_path,
    )
}

fn serve_live_probe_endpoint(listener: UnixListener, run_id: &str) -> Vec<Value> {
    let mut requests = Vec::new();
    for _ in 0..2 {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        let read = BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        if read == 0 {
            break;
        }
        let request = serde_json::from_str::<Value>(line.trim()).unwrap();
        let response = json!({
            "id": request.get("id").cloned().unwrap_or(Value::Null),
            "ok": true,
            "run_id": run_id,
            "run_status": "running",
            "run_status_reason": null,
            "run_mode": "finite",
            "initial_activation_limit": 1,
            "activation_limit": 1,
            "stop_attempt_limit": 3,
            "activations_used": 1,
            "event_cursor": 1,
            "context_generation": 2,
            "context": {
                "run_id": run_id,
                "run_status": "running",
                "activations": {},
                "ambiguous_deliveries": [],
                "flow_revisions": []
            }
        });
        writeln!(stream, "{response}").unwrap();
        stream.flush().unwrap();
        requests.push(request);
    }
    requests
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

impl Drop for AttachFixture {
    fn drop(&mut self) {
        let mut pids = fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name == "driver.pid" || name.starts_with("driver-") && name.ends_with(".pid")
            })
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .filter_map(|value| value.trim().parse::<i32>().ok())
            .collect::<Vec<_>>();
        pids.sort_unstable();
        pids.dedup();
        for pid in &pids {
            if process_exists(*pid) {
                unsafe {
                    libc::kill(*pid, libc::SIGTERM);
                }
            }
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) && pids.iter().copied().any(process_exists)
        {
            thread::sleep(Duration::from_millis(20));
        }
        for pid in &pids {
            if process_exists(*pid) {
                unsafe {
                    libc::kill(*pid, libc::SIGKILL);
                }
            }
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) && pids.iter().copied().any(process_exists)
        {
            thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            match self.prior_tmux.take() {
                Some(value) => std::env::set_var("HUMANIZE_TMUX_BIN", value),
                None => std::env::remove_var("HUMANIZE_TMUX_BIN"),
            }
            match self.prior_state_root.take() {
                Some(value) => std::env::set_var("HUMANIZE_STATE_ROOT", value),
                None => std::env::remove_var("HUMANIZE_STATE_ROOT"),
            }
        }
    }
}

fn fake_tmux(root: &Path) -> PathBuf {
    let path = root.join("fake-tmux");
    let script = format!(
        r#"#!/bin/sh
root='{}'
printf '%s\n' "$*" >> "$root/tmux.log"
last=''
target=''
buffer=''
previous=''
for arg in "$@"; do
  if test "$previous" = '-t'; then target="$arg"; fi
  if test "$previous" = '-b'; then buffer="$arg"; fi
  previous="$arg"
  last="$arg"
done
load_ready_environment() {{
  eval "set -- $1"
  if test "$1" = 'env'; then shift; fi
  while test "$#" -gt 0; do
    case "$1" in
      HUMANIZE_READY_RUN_ID=*|HUMANIZE_READY_ACTIVATION_ID=*|HUMANIZE_READY_ALLOCATION_GENERATION=*|HUMANIZE_READY_NONCE=*|HUMANIZE_PARTICIPANT_RUN_ID=*|HUMANIZE_PARTICIPANT_ACTIVATION_ID=*|HUMANIZE_PARTICIPANT_HANDLE=*|HUMANIZE_PARTICIPANT_CREDENTIAL=*|HUMANIZE_PARTICIPANT_BINDING_FILE=*) export "$1"; shift ;;
      *) break ;;
    esac
  done
}}
next_id() {{
  if test -f "$root/id.counter"; then id="$(( $(cat "$root/id.counter") + 1 ))"; else id='7'; fi
  printf '%s\n' "$id" > "$root/id.counter"
  printf '%s' "$id"
}}
handle_input() {{
  input="$1"
  case "$input" in
    *--run-id*)
      count='1'
      if test -f "$root/driver.launches"; then count="$(( $(cat "$root/driver.launches") + 1 ))"; fi
      printf '%s\n' "$count" > "$root/driver.launches"
      printf 'driver-launch-count=%s\n' "$count" >> "$root/tmux.log"
      pane="${{target##*.}}"
      TMUX_PANE="$pane" sh -c "$input" >> "$root/driver.out" 2>> "$root/driver.err" &
      printf '%s\n' "$!" > "$root/driver.pid"
      printf '%s\n' "$!" > "$root/driver-${{pane#%}}.pid"
      ;;
    *humanize-test-agent*)
      printf 'agent-launch-input\n' >> "$root/tmux.log"
      pane="${{target##*.}}"
      load_ready_environment "$input"
      pending="$root/hook-helper-${{pane#%}}-$$.pending"
      done="${{pending%.pending}}.done"
      : > "$pending"
      (
        printf '{{"hook_event_name":"SessionStart","session_id":"fake-native-%s"}}\n' "${{pane#%}}" |
          HUMANIZE_RUNS_DIR="$root/runs" TMUX_PANE="$pane" '{}' --agent-ready-hook --source codex_session_start
        mv "$pending" "$done"
      ) </dev/null >> "$root/hook.out" 2>> "$root/hook.err" &
      ;;
  esac
}}
case "$1" in
  has-session)
    test -f "$root/session.exists"
    ;;
  new-session)
    : > "$root/session.exists"
    window="$(next_id)"
    pane="$(next_id)"
    printf '%%%s\t%%%s\n' "$window" "$pane"
    ;;
  new-window)
    window="$(next_id)"
    pane="$(next_id)"
    printf '%%%s\t%%%s\n' "$window" "$pane"
    ;;
  split-window)
    pane="$(next_id)"
    printf '%%%s\n' "$pane"
    ;;
  display-message)
    if test -f "$root/killed-panes" && grep -Fqx "$target" "$root/killed-panes"; then exit 42; fi
    window="${{target#*:}}"
    window="${{window%%.*}}"
    pane="${{target##*.}}"
    printf '%s\t%s\t%s\t%s\n' 'host-a' "$window" 'flow-a' "$pane"
    ;;
  pipe-pane)
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" start "$root" "${{pane#%}}" "$last"
    ;;
  capture-pane)
    printf 'final capture for %s\n' "$target"
    ;;
  set-buffer)
    printf '%s' "$last" > "$root/tmux-buffer-$buffer"
    ;;
  paste-buffer)
    input="$(cat "$root/tmux-buffer-$buffer")"
    rm -f "$root/tmux-buffer-$buffer"
    handle_input "$input"
    ;;
  send-keys)
    handle_input "$last"
    case "$*" in
      *' C-u')
        pane="${{target##*.}}"
        : > "$root/prompt-${{pane#%}}.pending"
        ;;
      *' Enter')
        pane="${{target##*.}}"
        if test -f "$root/prompt-${{pane#%}}.pending"; then
          printf 'node-prompt-enter\n' >> "$root/tmux.log"
          rm -f "$root/prompt-${{pane#%}}.pending"
        fi
        ;;
    esac
    ;;
  kill-pane)
    if test -f "$root/killed-panes" && grep -Fqx "$target" "$root/killed-panes"; then exit 44; fi
    printf '%s\n' "$target" >> "$root/killed-panes"
    pane="${{target##*.}}"
    if test -f "$root/driver-${{pane#%}}.pid"; then
      kill "$(cat "$root/driver-${{pane#%}}.pid")" 2>/dev/null || true
    else
      "$root/fake-pipe-capture" stop "$root" "${{pane#%}}"
    fi
    ;;
esac
exit 0
"#,
        root.display(),
        env!("CARGO_BIN_EXE_humanize-plugin-mcp"),
    );
    fs::write(&path, script).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn locked_agent_flow() -> serde_json::Value {
    json!({
        "nodes": [{
            "id": "root",
            "action": {
                "driver": "agent",
                "prompt_ref": "prompt.start",
                "resource_refs": ["README.md"]
            }
        }],
        "resources": [
            {
                "path": "README.md",
                "kind": "readme",
                "content": "Use Humanize to inspect this repository."
            },
            {
                "path": "prompt.start",
                "kind": "prompt",
                "content": "Inspect the repository."
            }
        ]
    })
}

fn wait_for_text(path: &Path, needle: &str) -> String {
    let started = Instant::now();
    loop {
        if let Ok(value) = fs::read_to_string(path)
            && value.contains(needle)
        {
            return value;
        }
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "{}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn process_exists(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}
