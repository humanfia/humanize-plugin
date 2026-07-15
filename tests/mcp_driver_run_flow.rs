#[path = "mcp_driver_run_flow/launch.rs"]
mod launch;
#[path = "mcp_driver_run_flow/recovery.rs"]
mod recovery;
mod support;

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::adapters::tmux::SystemCommandRunner;
use humanize_plugin::mcp::{McpServer, TmuxExecutionDefaults};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use support::driver_tmux::{ControlledTmuxFixture, fake_tmux_with_sequential_panes};
use support::mcp::{call_tool, lock_flow, structured};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn driver_signal_finalizes_capture_and_releases_node_and_operator_panes() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-driver-signal-lifecycle")
        .join(std::process::id().to_string());
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    set_test_state_root(&root);
    let tmux_control = ControlledTmuxFixture::new(&root);
    let fake_tmux = fake_tmux_with_sequential_panes(&tmux_control);
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
            "run_id": "run-driver-signal",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(tmux_control.wait_for_hooks());
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);
    assert_eq!(structured(&started)["ok"], true, "{started}");

    let driver_pid: i32 = wait_for_file(&root.join("driver.pid"))
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(driver_pid, libc::SIGTERM) }, 0);
    wait_for_process_exit(driver_pid);

    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-driver-signal")
        .unwrap();
    let manifest = private_run_assets(&run_root);
    assert_eq!(manifest["activations"]["root"]["capture_phase"], "complete");
    assert_eq!(
        manifest["activations"]["root"]["resource_cleanup_status"],
        "complete"
    );
    let calls = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert!(calls.contains("kill-pane -t host-a:%7.%9"), "{calls}");
    assert!(calls.contains("kill-pane -t host-a:%7.%8"), "{calls}");
}

#[test]
fn driver_owned_apply_flow_update_proxies_exact_package_and_apply_mode() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-apply-update")
        .join(std::process::id().to_string());
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    set_test_state_root(&root);
    let _tmux_control = ControlledTmuxFixture::new(&root);
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
    let (initial_lock_id, initial_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-driver-update",
            "flow_lock_id": initial_lock_id,
            "content_hash": initial_hash,
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let proposed = call_tool(
        &mut server,
        3,
        "propose_flow_update",
        json!({
            "flow": updated_agent_flow(),
            "apply_mode": "checkpoint_restart",
            "summary": "Switch to the updated driver-owned flow."
        }),
    );
    assert_eq!(structured(&proposed)["ok"], true);
    let lock_id = structured(&proposed)["flow_lock_id"].as_str().unwrap();
    let content_hash = structured(&proposed)["content_hash"].as_str().unwrap();

    let applied = call_tool(
        &mut server,
        4,
        "apply_flow_update",
        json!({
            "run_id": "run-driver-update",
            "flow_lock_id": lock_id,
            "content_hash": content_hash
        }),
    );
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);

    assert_eq!(structured(&applied)["ok"], true);
    assert_eq!(structured(&applied)["apply_mode"], "checkpoint_restart");
    assert_eq!(structured(&applied)["flow_lock_id"], lock_id);
    assert_eq!(structured(&applied)["content_hash"], content_hash);

    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-driver-update")
        .unwrap();
    let revision = driver_revision_with_hash(&run_root, content_hash);
    assert_eq!(revision["flow_lock"]["lock_id"], lock_id);
    assert_eq!(revision["flow_lock"]["content_hash"], content_hash);
    assert_eq!(revision["flow_lock"]["lock_id"], lock_id);
    assert_eq!(revision["flow_lock"]["content_hash"], content_hash);
    assert!(revision["review_id"].as_str().is_some());
    let manifest = private_run_assets(&run_root);
    let current_revision_id = manifest["flow"]["current_revision_id"].as_str().unwrap();
    let current_revision = manifest["flow"]["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|revision| revision["revision_id"] == current_revision_id)
        .unwrap();
    assert_eq!(current_revision["flow_lock_id"], lock_id);
    assert_eq!(current_revision["content_hash"], content_hash);
    let status = call_tool(
        &mut server,
        5,
        "run_status",
        json!({
            "run_id": "run-driver-update"
        }),
    );
    let flow_revisions = structured(&status)["context"]["flow_revisions"]
        .as_array()
        .unwrap();
    let applied_revision = flow_revisions
        .iter()
        .find(|revision| revision["content_hash"] == content_hash)
        .expect("updated flow revision should be present");
    assert_eq!(applied_revision["mode"], "checkpoint_restart");
    shutdown_driver_for_run(&run_root, "run-driver-update");
}

#[test]
fn production_run_flow_rejects_unlocked_forms_before_local_state() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-reject-unlocked")
        .join(std::process::id().to_string());
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    set_test_state_root(&root);
    let mut server = McpServer::with_tmux_runner_run_asset_store_and_execution_defaults(
        SystemCommandRunner,
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs"))),
        TmuxExecutionDefaults {
            session: Some("host-a".into()),
            window: Some("flow-a".into()),
            agent_command: Some("humanize-test-agent".into()),
        },
    );

    let result = call_tool(
        &mut server,
        1,
        "run_flow",
        json!({
            "run_id": "run-unlocked-rejected",
            "nodes": [{ "id": "root" }],
        }),
    );

    assert_eq!(structured(&result)["ok"], false);
    assert_eq!(
        structured(&result)["error"],
        "flow_lock_id is required for driver run_flow"
    );
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-unlocked-rejected")
        .unwrap();
    assert!(!run_root.exists());
}

#[test]
fn production_start_run_is_hidden_before_state_creation() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-reject-start-run")
        .join(std::process::id().to_string());
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    set_test_state_root(&root);
    let mut server = McpServer::with_tmux_runner_run_asset_store_and_execution_defaults(
        SystemCommandRunner,
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs"))),
        TmuxExecutionDefaults {
            session: Some("host-a".into()),
            window: Some("flow-a".into()),
            agent_command: Some("humanize-test-agent".into()),
        },
    );

    let result = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-start-rejected",
            "nodes": [{ "id": "root" }]
        }),
    );

    assert_eq!(result["error"]["code"], -32602);
    assert_eq!(result["error"]["message"], "unknown tool");
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-start-rejected")
        .unwrap();
    assert!(!run_root.exists());
}

#[test]
fn startup_send_failure_cleans_private_ipc_artifacts_and_allows_retry() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-startup-rollback")
        .join(std::process::id().to_string());
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    set_test_state_root(&root);
    let tmux_control = ControlledTmuxFixture::new(&root);
    let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");

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

    let failing_tmux = fake_tmux_with_driver_send_failure(&root);
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &failing_tmux);
    }
    let failed = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-driver-retry",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-driver-retry")
        .unwrap();
    assert!(failed["error"].is_object());
    assert_eq!(failed["error"]["message"], "runtime driver launch failed");
    assert_model_response_omits_pane_identity(&failed, &["%8"]);
    let diagnostics = private_mcp_diagnostics(&run_root);
    assert!(
        diagnostics.contains("driver_launch_failed"),
        "{diagnostics}"
    );
    assert!(diagnostics.contains("%8"), "{diagnostics}");
    assert!(!diagnostics.contains("--driver-pane-id"), "{diagnostics}");
    assert!(
        diagnostics.contains("operation=paste-buffer"),
        "{diagnostics}"
    );
    assert!(
        diagnostics.contains("command_hash=sha256:"),
        "{diagnostics}"
    );
    assert!(diagnostics.contains("command_length="), "{diagnostics}");
    assert_eq!(
        file_mode(private_driver_dir(&run_root).join("mcp-diagnostics.jsonl")),
        0o600
    );
    assert!(!private_driver_dir(&run_root).join("ipc-token").exists());
    assert!(!private_driver_dir(&run_root).join("ipc.json").exists());
    assert!(!run_root.join("driver").exists());

    let working_tmux = fake_tmux(&root);
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &working_tmux);
    }
    let retried = call_tool(
        &mut server,
        3,
        "run_flow",
        json!({
            "run_id": "run-driver-retry",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(tmux_control.wait_for_hooks());
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);

    assert_eq!(structured(&retried)["ok"], true, "{retried}");
    assert_eq!(file_mode(private_driver_dir(&run_root)), 0o700);
    assert_eq!(
        file_mode(private_driver_dir(&run_root).join("ipc-token")),
        0o600
    );
    shutdown_driver_for_run(&run_root, "run-driver-retry");
}

#[test]
fn startup_readiness_failure_cleans_ipc_artifacts_and_allows_retry() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-readiness-rollback")
        .join(std::process::id().to_string());
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    set_test_state_root(&root);
    let tmux_control = ControlledTmuxFixture::new(&root);
    let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
    let prior_timeout = std::env::var_os("HUMANIZE_DRIVER_READY_TIMEOUT_MS");

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

    let quiet_tmux = fake_tmux_without_driver_start(&root);
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &quiet_tmux);
        std::env::set_var("HUMANIZE_DRIVER_READY_TIMEOUT_MS", "50");
    }
    let started_at = Instant::now();
    let failed = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-driver-readiness-retry",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(started_at.elapsed() < Duration::from_secs(4));
    assert!(failed["error"].is_object());
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-driver-readiness-retry")
        .unwrap();
    assert!(!private_driver_dir(&run_root).join("ipc-token").exists());
    assert!(!private_driver_dir(&run_root).join("ipc.json").exists());
    assert!(!run_root.join("driver").exists());

    let working_tmux = fake_tmux(&root);
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &working_tmux);
    }
    let retried = call_tool(
        &mut server,
        3,
        "run_flow",
        json!({
            "run_id": "run-driver-readiness-retry",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(tmux_control.wait_for_hooks());
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);
    restore_env("HUMANIZE_DRIVER_READY_TIMEOUT_MS", prior_timeout);

    assert_eq!(structured(&retried)["ok"], true, "{retried}");
    shutdown_driver_for_run(&run_root, "run-driver-readiness-retry");
}

#[test]
fn startup_cleanup_does_not_probe_or_kill_same_run_id_in_another_runs_root() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-cross-store-cleanup")
        .join(std::process::id().to_string());
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    set_test_state_root(&root);
    let run_id = "run-cross-store-cleanup";
    let foreign_runs_root = root.join("foreign/runs");
    let foreign_store =
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(foreign_runs_root.clone()));
    let foreign_manifest = foreign_store.start_run_manifest(run_id).unwrap();
    seed_private_owned_pane(
        &root,
        &foreign_runs_root,
        &foreign_manifest.root,
        run_id,
        "%77",
    );

    let local_runs_root = root.join("local/runs");
    let local_store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(local_runs_root.clone()));
    let local_manifest = local_store.start_run_manifest(run_id).unwrap();
    seed_private_owned_pane(&root, &local_runs_root, &local_manifest.root, run_id, "%66");
    let mut server = McpServer::with_tmux_runner_run_asset_store_and_execution_defaults(
        SystemCommandRunner,
        local_store,
        TmuxExecutionDefaults {
            session: Some("host-a".into()),
            window: Some("flow-a".into()),
            agent_command: Some("humanize-test-agent".into()),
        },
    );
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
    let prior_timeout = std::env::var_os("HUMANIZE_DRIVER_READY_TIMEOUT_MS");
    let quiet_tmux = fake_tmux_for_cross_store_cleanup(&root);
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &quiet_tmux);
        std::env::set_var("HUMANIZE_DRIVER_READY_TIMEOUT_MS", "50");
    }

    let failed = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": run_id,
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);
    restore_env("HUMANIZE_DRIVER_READY_TIMEOUT_MS", prior_timeout);

    assert!(failed["error"].is_object(), "{failed}");
    let tmux_log = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert!(
        !tmux_log.contains("display-message -p -t host-a:%7.%77"),
        "foreign pane was probed during local cleanup: {tmux_log}"
    );
    assert!(
        !tmux_log.contains("kill-pane -t host-a:%7.%77"),
        "foreign pane was killed during local cleanup: {tmux_log}"
    );
}

#[test]
fn bind_failure_cleans_driver_and_node_panes_and_allows_retry() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-bind-rollback")
        .join(std::process::id().to_string());
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    set_test_state_root(&root);
    let tmux_control = ControlledTmuxFixture::new(&root);
    let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
    let prior_driver_bin = std::env::var_os("HUMANIZE_DRIVER_BIN");
    let prior_driver_event_fault = std::env::var_os("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS");
    let prior_driver_event_kind = std::env::var_os("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND");
    let prior_agent_timeout = std::env::var_os("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS");

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

    let failing_tmux = fake_tmux_with_driver_pid_cleanup(&tmux_control);
    let fault_marker = root.join("fail-node-pane-event");
    fs::write(&fault_marker, "fail").unwrap();
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &failing_tmux);
        std::env::set_var(
            "HUMANIZE_DRIVER_BIN",
            env!("CARGO_BIN_EXE_humanize-plugin-driver"),
        );
        std::env::set_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", &fault_marker);
        std::env::set_var(
            "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
            "tmux_pane_allocated",
        );
        std::env::set_var("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50");
    }
    let failed = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-driver-bind-retry",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert_eq!(structured(&failed)["ok"], false);
    let calls = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert!(calls.contains("split-window"));
    assert!(calls.contains("kill-pane -t host-a:%7.%8"));
    assert!(calls.contains("kill-pane -t host-a:%7.%9"));
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-driver-bind-retry")
        .unwrap();
    assert!(!private_driver_dir(&run_root).join("ipc-token").exists());
    assert!(!private_driver_dir(&run_root).join("ipc.json").exists());
    assert!(!run_root.join("driver").exists());

    fs::remove_file(&fault_marker).unwrap();
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &failing_tmux);
    }
    let retried = call_tool(
        &mut server,
        3,
        "run_flow",
        json!({
            "run_id": "run-driver-bind-retry",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);
    restore_env("HUMANIZE_DRIVER_BIN", prior_driver_bin);
    restore_env(
        "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS",
        prior_driver_event_fault,
    );
    restore_env(
        "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
        prior_driver_event_kind,
    );
    restore_env(
        "HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS",
        prior_agent_timeout,
    );

    assert_eq!(structured(&retried)["ok"], true);
    shutdown_driver_for_run(&run_root, "run-driver-bind-retry");
}

#[test]
fn existing_run_proxy_cleans_failed_activation_pane_before_resume_retry() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-proxy-cleanup")
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
    let fault_marker = root.join("fail-activated-pane-event");
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
        std::env::set_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", &fault_marker);
        std::env::set_var(
            "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
            "tmux_pane_allocated",
        );
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
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_manual_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-driver-proxy-cleanup",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a",
                "agent_command": "humanize-test-agent"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true, "{started}");
    assert_eq!(
        structured(&started)["tmux"]["panes"][0]["activation_id"],
        "root"
    );
    assert!(
        structured(&started)["tmux"]["panes"][0]
            .get("pane_id")
            .is_none()
    );
    fs::write(&fault_marker, "fail").unwrap();

    let failed = call_tool(
        &mut server,
        3,
        "activate_node",
        json!({
            "run_id": "run-driver-proxy-cleanup",
            "node_id": "manual"
        }),
    );
    assert_eq!(structured(&failed)["ok"], false, "{failed}");
    assert_cleanup_response_sanitized(structured(&failed), 1, 0, &["%10"]);
    let calls_after_failure = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert!(
        calls_after_failure.contains("kill-pane -t host-a:%7.%10"),
        "{calls_after_failure}"
    );
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-driver-proxy-cleanup")
        .unwrap();
    assert!(private_mcp_diagnostics(&run_root).contains("%10"));

    fs::remove_file(&fault_marker).unwrap();
    let resumed = call_tool(
        &mut server,
        4,
        "resume_run",
        json!({
            "run_id": "run-driver-proxy-cleanup"
        }),
    );
    assert!(tmux_control.wait_for_hooks());
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
    assert_eq!(
        structured(&resumed)["tmux_allocations"][0]["activation_id"],
        "manual"
    );
    assert!(
        structured(&resumed)["tmux_allocations"][0]
            .get("pane_id")
            .is_none()
    );
    let calls = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert_eq!(calls.matches("split-window").count(), 3, "{calls}");
    shutdown_driver_for_run(&run_root, "run-driver-proxy-cleanup");
}

#[test]
fn bind_actuation_failure_cleans_all_real_driver_panes_and_retries_same_run() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-actuation-cleanup")
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
    let prior_agent_timeout = std::env::var_os("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS");
    let fake_tmux = fake_tmux_with_sequential_panes(&tmux_control);
    let fault_marker = root.join("fail-agent-launch-event");
    fs::write(&fault_marker, "fail").unwrap();
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
        std::env::set_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", &fault_marker);
        std::env::set_var(
            "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
            "agent_launch_submitted",
        );
        std::env::set_var("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50");
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
    let failed = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-driver-actuation-cleanup",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );

    assert_eq!(structured(&failed)["ok"], false, "{failed}");
    assert_lifecycle_cleanup_response_sanitized(structured(&failed), &["%8", "%9"]);
    let calls_after_failure = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert_eq!(
        calls_after_failure
            .lines()
            .filter(|line| *line == "kill-pane -t host-a:%7.%9")
            .count(),
        1,
        "{calls_after_failure}"
    );
    assert_eq!(
        calls_after_failure
            .lines()
            .filter(|line| *line == "kill-pane -t host-a:%7.%8")
            .count(),
        1,
        "{calls_after_failure}"
    );
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-driver-actuation-cleanup")
        .unwrap();

    fs::remove_file(&fault_marker).unwrap();
    let retried = call_tool(
        &mut server,
        3,
        "run_flow",
        json!({
            "run_id": "run-driver-actuation-cleanup",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
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
    restore_env(
        "HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS",
        prior_agent_timeout,
    );

    assert_eq!(structured(&retried)["ok"], true, "{retried}");
    assert_eq!(structured(&retried)["run_status"], "paused", "{retried}");
    let status = call_tool(
        &mut server,
        4,
        "run_status",
        json!({ "run_id": "run-driver-actuation-cleanup" }),
    );
    let barrier = &structured(&status)["context"]["ambiguous_deliveries"][0];
    assert_eq!(barrier["role"], "agent_launch", "{status}");
    let resumed = call_tool(
        &mut server,
        5,
        "resume_run",
        json!({
            "run_id": "run-driver-actuation-cleanup",
            "delivery_resolution": {
                "started_event_sequence": barrier["started_event_sequence"],
                "outcome": "not_submitted",
                "evidence": "the failed bind cleanup destroyed the receiver pane"
            }
        }),
    );
    assert_eq!(structured(&resumed)["ok"], true, "{resumed}");
    assert_eq!(
        structured(&resumed)["tmux_allocations"][0]["activation_id"],
        "root",
        "{resumed}"
    );
    assert!(
        structured(&resumed)["tmux_allocations"][0]
            .get("pane_id")
            .is_none(),
        "{resumed}"
    );
    shutdown_driver_for_run(&run_root, "run-driver-actuation-cleanup");
}

#[test]
fn bind_cleanup_attempts_every_pane_and_reports_kill_failure() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-kill-failure")
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
    let prior_kill_failure = std::env::var_os("HUMANIZE_TEST_FAIL_KILL_PANE");
    let fake_tmux = fake_tmux_with_sequential_panes(&tmux_control);
    let fault_marker = root.join("fail-agent-submitted-event");
    fs::write(&fault_marker, "fail").unwrap();
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
        std::env::set_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", &fault_marker);
        std::env::set_var(
            "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
            "agent_launch_submitted",
        );
        std::env::set_var("HUMANIZE_TEST_FAIL_KILL_PANE", "host-a:%7.%9");
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
    let failed = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-driver-kill-failure",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
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
    restore_env("HUMANIZE_TEST_FAIL_KILL_PANE", prior_kill_failure);

    assert_eq!(structured(&failed)["ok"], false, "{failed}");
    assert_cleanup_response_sanitized(structured(&failed), 1, 1, &["%8", "%9"]);
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-driver-kill-failure")
        .unwrap();
    let diagnostics = private_mcp_diagnostics(&run_root);
    assert!(diagnostics.contains("%9"), "{diagnostics}");
    assert!(diagnostics.contains("43"), "{diagnostics}");
    let calls = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert!(calls.contains("kill-pane -t host-a:%7.%9"), "{calls}");
    assert_eq!(
        calls
            .lines()
            .filter(|line| *line == "kill-pane -t host-a:%7.%8")
            .count(),
        1,
        "{calls}"
    );
    fs::write(root.join("pipe-9.stop"), "stopped\n").unwrap();
    thread::sleep(Duration::from_millis(100));
}

#[test]
fn retry_reconciles_externally_cleaned_pane_when_cleanup_intent_could_not_persist() {
    let _guard = lock_test_environment();
    let root = std::env::temp_dir()
        .join("humanize-plugin-mcp-driver-release-recovery")
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
    let prior_fault_after_agent = std::env::var_os("HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT");
    let prior_agent_timeout = std::env::var_os("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS");
    let fake_tmux = fake_tmux_with_sequential_panes(&tmux_control);
    let fault_marker = root.join("fail-agent-and-release-events");
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
        std::env::set_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", &fault_marker);
        std::env::remove_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND");
        std::env::set_var(
            "HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT",
            &fault_marker,
        );
        std::env::set_var("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50");
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
    let failed = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-driver-release-recovery",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert_eq!(structured(&failed)["ok"], false, "{failed}");
    assert_cleanup_response_sanitized(structured(&failed), 1, 0, &["%8", "%9"]);
    assert!(root.join("fail-agent-and-release-events").exists());

    fs::remove_file(&fault_marker).unwrap();
    unsafe {
        std::env::remove_var("HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT");
    }
    let retried = call_tool(
        &mut server,
        3,
        "run_flow",
        json!({
            "run_id": "run-driver-release-recovery",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
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
    restore_env(
        "HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT",
        prior_fault_after_agent,
    );
    restore_env(
        "HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS",
        prior_agent_timeout,
    );

    assert_eq!(structured(&retried)["ok"], true, "{retried}");
    assert_eq!(structured(&retried)["run_status"], "paused", "{retried}");
    let context = call_tool(
        &mut server,
        4,
        "get_context",
        json!({ "run_id": "run-driver-release-recovery" }),
    );
    let started_event_sequence =
        structured(&context)["context"]["ambiguous_deliveries"][0]["started_event_sequence"]
            .as_u64()
            .unwrap();
    let resumed = call_tool(
        &mut server,
        5,
        "resume_run",
        json!({
            "run_id": "run-driver-release-recovery",
            "delivery_resolution": {
                "started_event_sequence": started_event_sequence,
                "outcome": "submitted",
                "evidence": "the old pane received Enter before lifecycle cleanup released it"
            }
        }),
    );
    assert!(tmux_control.wait_for_hooks());
    assert_eq!(structured(&resumed)["ok"], true, "{resumed}");
    assert_eq!(
        structured(&resumed)["tmux_allocations"][0]["activation_id"],
        "root",
        "{resumed}"
    );
    assert!(
        structured(&resumed)["tmux_allocations"][0]
            .get("pane_id")
            .is_none(),
        "{resumed}"
    );
    assert_eq!(
        structured(&resumed)["tmux_allocations"][0]["allocation_generation"],
        1,
        "{resumed}"
    );
    assert_eq!(
        structured(&resumed)["actuation"]["warnings"][0]["status"],
        "readiness_pending",
        "{resumed}"
    );
    let calls = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert_eq!(calls.matches("humanize-test-agent").count(), 2);
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root("run-driver-release-recovery")
        .unwrap();
    shutdown_driver_for_run(&run_root, "run-driver-release-recovery");
}

fn assert_cleanup_response_sanitized(
    response: &serde_json::Value,
    attempted: u64,
    failed: u64,
    forbidden_pane_ids: &[&str],
) {
    assert_eq!(
        response["error"]["cleanup"]["attempted"], attempted,
        "{response}"
    );
    assert_eq!(response["error"]["cleanup"]["failed"], failed, "{response}");
    assert!(
        response["error"].get("tmux_cleanup").is_none(),
        "{response}"
    );
    assert_model_response_omits_pane_identity(response, forbidden_pane_ids);
}

fn assert_lifecycle_cleanup_response_sanitized(
    response: &serde_json::Value,
    forbidden_pane_ids: &[&str],
) {
    if let Some(cleanup) = response["error"].get("cleanup") {
        assert!(cleanup.get("actions").is_none(), "{response}");
        assert!(cleanup.get("status").is_some(), "{response}");
    }
    assert!(
        response["error"].get("tmux_cleanup").is_none(),
        "{response}"
    );
    assert_model_response_omits_pane_identity(response, forbidden_pane_ids);
}

fn assert_model_response_omits_pane_identity(
    response: &serde_json::Value,
    forbidden_pane_ids: &[&str],
) {
    let rendered = response.to_string();
    assert!(!rendered.contains("pane_id"), "{response}");
    assert!(!rendered.contains("driver-pane-id"), "{response}");
    assert!(!rendered.contains("auth-token-file"), "{response}");
    for pane_id in forbidden_pane_ids {
        assert!(!rendered.contains(pane_id), "{response}");
    }
}

fn private_mcp_diagnostics(run_root: &Path) -> String {
    fs::read_to_string(private_driver_dir(run_root).join("mcp-diagnostics.jsonl"))
        .expect("private MCP diagnostics should be persisted")
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
handle_input() {{
  input="$1"
  case "$input" in
    *humanize-plugin-driver*--run-id*)
      pane="${{target##*.}}"
      TMUX_PANE="$pane" sh -c "$input" >> "$root/driver.out" 2>> "$root/driver.err" &
      ;;
    *humanize-test-agent*)
      load_ready_environment "$input"
      pane="${{target##*.}}"
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
    exit 1
    ;;
  new-session)
    printf '%s\t%s\n' '%7' '%8'
    ;;
  split-window)
    printf '%s\n' '%9'
    ;;
  display-message)
    pane='%8'
    case "$*" in
      *.%9*) pane='%9' ;;
    esac
    printf '%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' "$pane"
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
    ;;
  kill-pane)
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" stop "$root" "${{pane#%}}"
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

fn fake_tmux_with_driver_send_failure(root: &Path) -> PathBuf {
    let path = root.join("fake-tmux-send-fails");
    let script = format!(
        r#"#!/bin/sh
root='{}'
printf '%s\n' "$*" >> "$root/tmux.log"
last=''
buffer=''
previous=''
for arg in "$@"; do
  if test "$previous" = '-b'; then buffer="$arg"; fi
  previous="$arg"
  last="$arg"
done
case "$1" in
  has-session)
    exit 1
    ;;
  new-session)
    printf '%s\t%s\n' '%7' '%8'
    ;;
  display-message)
    printf '%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' '%8'
    ;;
  set-buffer)
    printf '%s' "$last" > "$root/tmux-buffer-$buffer"
    ;;
  paste-buffer)
    input="$(cat "$root/tmux-buffer-$buffer")"
    rm -f "$root/tmux-buffer-$buffer"
    case "$input" in
      *humanize-plugin-driver*) exit 42 ;;
    esac
    ;;
  send-keys)
    case "$*" in
      *humanize-plugin-driver*)
        exit 42
        ;;
    esac
    ;;
esac
exit 0
"#,
        root.display()
    );
    fs::write(&path, script).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_tmux_without_driver_start(root: &Path) -> PathBuf {
    let path = root.join("fake-tmux-no-driver");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$1" in
  has-session)
    exit 1
    ;;
  new-session)
    printf '%s\t%s\n' '%7' '%8'
    ;;
  display-message)
    printf '%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' '%8'
    ;;
esac
exit 0
"#,
        root.join("tmux.log").display()
    );
    fs::write(&path, script).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_tmux_for_cross_store_cleanup(root: &Path) -> PathBuf {
    let path = root.join("fake-tmux-cross-store-cleanup");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
target=''
previous=''
for arg in "$@"; do
  if test "$previous" = '-t'; then target="$arg"; fi
  previous="$arg"
done
case "$1" in
  has-session)
    exit 1
    ;;
  new-session)
    printf '%s\t%s\n' '%7' '%8'
    ;;
  display-message)
    window="${{target#*:}}"
    window="${{window%%.*}}"
    pane="${{target##*.}}"
    printf '%s\t%s\t%s\t%s\n' 'host-a' "$window" 'flow-a' "$pane"
    ;;
esac
exit 0
"#,
        root.join("tmux.log").display()
    );
    fs::write(&path, script).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn seed_private_owned_pane(
    state_root: &Path,
    runs_root: &Path,
    run_root: &Path,
    run_id: &str,
    pane_id: &str,
) {
    let runtime_root = state_root.join("runtime");
    fs::create_dir_all(&runtime_root).unwrap();
    fs::set_permissions(&runtime_root, fs::Permissions::from_mode(0o700)).unwrap();
    let absolute_run_root = std::path::absolute(run_root).unwrap();
    let identity = absolute_run_root.to_string_lossy();
    let private_run_root = runtime_root.join(format!("r{:016x}", stable_hash(&identity)));
    fs::create_dir_all(private_run_root.join("driver")).unwrap();
    fs::set_permissions(&private_run_root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(
        private_run_root.join("driver"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let identity_path = private_run_root.join("identity.json");
    fs::write(
        &identity_path,
        serde_json::to_vec_pretty(&json!({
            "schema": "humanize.private_run_identity.v1",
            "run_id": run_id,
            "public_run_root": absolute_run_root,
            "runs_root": std::path::absolute(runs_root).unwrap()
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&identity_path, fs::Permissions::from_mode(0o600)).unwrap();
    let events_path = private_run_root.join("driver/driver-events.jsonl");
    writeln!(
        fs::File::create(&events_path).unwrap(),
        "{}",
        json!({
            "seq": 1,
            "at_ms": 1,
            "kind": "tmux_pane_allocated",
            "payload": {
                "activation_id": "root",
                "pane": {
                    "session_id": "host-a",
                    "window_id": "%7",
                    "window_name": "flow-a",
                    "pane_id": pane_id,
                    "allocation_generation": 0
                }
            }
        })
    )
    .unwrap();
    fs::set_permissions(&events_path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn fake_tmux_with_driver_pid_cleanup(control: &ControlledTmuxFixture) -> PathBuf {
    fake_tmux_with_sequential_panes(control)
}

fn restore_env(name: &str, prior: Option<std::ffi::OsString>) {
    unsafe {
        match prior {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}

fn lock_test_environment() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn set_test_state_root(root: &Path) {
    unsafe {
        std::env::set_var("HUMANIZE_STATE_ROOT", root);
    }
}

fn wait_for_file(path: &Path) -> String {
    let started = Instant::now();
    loop {
        if let Ok(value) = fs::read_to_string(path) {
            return value;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_text(path: &Path, expected: &str) -> String {
    let started = Instant::now();
    loop {
        if let Ok(value) = fs::read_to_string(path)
            && value.contains(expected)
        {
            return value;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timed out waiting for {expected} in {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_process_exit(pid: i32) {
    let started = Instant::now();
    loop {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "driver process {pid} did not exit"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn file_mode(path: impl AsRef<Path>) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn private_driver_dir(run_root: &Path) -> PathBuf {
    let runtime_root = run_root
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("runtime");
    let identity = std::path::absolute(run_root)
        .unwrap_or_else(|_| run_root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    runtime_root
        .join(format!("r{:016x}", stable_hash(&identity)))
        .join("driver")
}

fn private_run_assets(run_root: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(private_driver_dir(run_root).join("run-assets.json")).unwrap())
        .unwrap()
}

fn read_single_private_binding(private_run_root: &Path) -> Value {
    let mut entries = fs::read_dir(private_run_root.join("bindings"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries.len(), 1);
    let mut binding: Value = serde_json::from_slice(&fs::read(&entries[0]).unwrap()).unwrap();
    binding["__path"] = Value::String(entries[0].to_string_lossy().into_owned());
    binding
}

fn collect_public_files(root: &Path) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    collect_public_files_inner(root, root, &mut files);
    files
}

fn collect_public_files_inner(root: &Path, path: &Path, files: &mut BTreeSet<String>) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(
        !metadata.file_type().is_symlink(),
        "public tree contains a symlink"
    );
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            collect_public_files_inner(root, &entry, files);
        }
        return;
    }
    assert!(metadata.is_file(), "public tree contains a special file");
    files.insert(
        path.strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/"),
    );
}

fn collect_tree_bytes(root: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    collect_tree_bytes_inner(root, root, &mut bytes);
    bytes
}

fn collect_tree_bytes_inner(root: &Path, path: &Path, bytes: &mut Vec<u8>) {
    let metadata = fs::symlink_metadata(path).unwrap();
    let relative = path.strip_prefix(root).unwrap_or(path);
    bytes.extend_from_slice(relative.as_os_str().as_encoded_bytes());
    bytes.push(b'\n');
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            collect_tree_bytes_inner(root, &entry, bytes);
        }
    } else {
        assert!(metadata.is_file(), "public tree contains a special file");
        bytes.extend(fs::read(path).unwrap());
        bytes.push(b'\n');
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn shutdown_driver(run_root: &Path) {
    shutdown_driver_for_run(run_root, "run-driver-flow");
}

fn shutdown_driver_for_run(run_root: &Path, run_id: &str) {
    let driver_dir = private_driver_dir(run_root);
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(driver_dir.join("ipc.json")).unwrap()).unwrap();
    let socket_path = driver_dir
        .parent()
        .unwrap()
        .join(metadata["socket_path"].as_str().unwrap());
    let token_path = driver_dir.join(metadata["auth_token_path"].as_str().unwrap());
    let token = fs::read_to_string(token_path).unwrap();
    let mut stream = UnixStream::connect(socket_path).unwrap();
    let request = json!({
        "id": "shutdown",
        "token": token.trim(),
        "op": "shutdown",
        "run_id": run_id
    });
    stream
        .write_all((request.to_string() + "\n").as_bytes())
        .unwrap();
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], true, "{response}");
}

fn driver_revision_with_hash(run_root: &Path, content_hash: &str) -> serde_json::Value {
    let revisions_dir = private_driver_dir(run_root).join("revisions");
    for entry in fs::read_dir(revisions_dir).unwrap() {
        let path = entry.unwrap().path();
        let revision: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        if revision["flow_lock"]["content_hash"] == content_hash {
            return revision;
        }
    }
    panic!("driver revision not found for {content_hash}");
}

fn locked_agent_flow() -> serde_json::Value {
    json!({
        "nodes": [
            {
                "id": "root",
                "action": {
                    "driver": "agent",
                    "prompt_ref": "prompt.start",
                    "resource_refs": ["README.md"]
                }
            }
        ],
        "resources": [
            {
                "path": "README.md",
                "kind": "readme",
                "content": "Use Humanize to audit this library without editing files."
            },
            {
                "path": "prompt.start",
                "kind": "prompt",
                "content": "Inspect the repository."
            }
        ]
    })
}

fn locked_manual_flow() -> serde_json::Value {
    json!({
        "nodes": [
            {
                "id": "root",
                "action": {
                    "driver": "human"
                }
            },
            {
                "id": "manual",
                "action": {
                    "driver": "human"
                }
            }
        ],
        "routes": [
            {
                "predicate": {
                    "op": "exists",
                    "fact": {"kind": "artifact", "key": "never"}
                },
                "activate": "manual"
            }
        ],
        "resources": [
            {
                "path": "README.md",
                "kind": "readme",
                "content": "Driver proxy cleanup fixture."
            }
        ]
    })
}

fn updated_agent_flow() -> serde_json::Value {
    json!({
        "nodes": [
            {
                "id": "root",
                "action": {
                    "driver": "agent",
                    "prompt_ref": "prompt.start",
                    "resource_refs": ["README.md"]
                }
            }
        ],
        "resources": [
            {
                "path": "README.md",
                "kind": "readme",
                "content": "Use Humanize to audit this library with the updated flow."
            },
            {
                "path": "prompt.start",
                "kind": "prompt",
                "content": "Inspect the updated repository context."
            }
        ]
    })
}
