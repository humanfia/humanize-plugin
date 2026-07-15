mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::adapters::tmux::SystemCommandRunner;
use humanize_plugin::driver::DriverClient;
use humanize_plugin::input_ledger::MachineInputRecord;
use humanize_plugin::mcp::{McpServer, TmuxExecutionDefaults};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::json;

use support::driver_tmux::{ControlledTmuxFixture, fake_tmux_with_sequential_panes};
use support::mcp::{call_tool, lock_flow, structured};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn participant_message_ambiguity_resolves_only_through_resume_and_survives_restart() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = test_root("message-ambiguity");
    let tmux_control = ControlledTmuxFixture::new(&root);
    let fake_tmux = fake_tmux_with_sequential_panes(&tmux_control);
    let fault_marker = root.join("fail-message-submitted");
    let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
    let prior_state_root = std::env::var_os("HUMANIZE_STATE_ROOT");
    let prior_fault = std::env::var_os("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS");
    let prior_kind = std::env::var_os("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND");
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
        std::env::set_var("HUMANIZE_STATE_ROOT", &root);
        std::env::set_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", &fault_marker);
        std::env::set_var(
            "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
            "participant_message_submitted",
        );
    }

    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")));
    let mut server = McpServer::with_tmux_runner_run_asset_store_and_execution_defaults(
        SystemCommandRunner,
        store.clone(),
        TmuxExecutionDefaults {
            session: Some("host-a".into()),
            window: Some("flow-a".into()),
            agent_command: Some("humanize-test-agent".into()),
        },
    );
    let (lock_id, content_hash) = lock_flow(&mut server, 1, agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-message",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(tmux_control.wait_for_hooks());
    assert_eq!(structured(&started)["ok"], true, "{started}");
    let paused = call_tool(
        &mut server,
        3,
        "pause_run",
        json!({ "run_id": "run-message" }),
    );
    assert_eq!(structured(&paused)["run_status"], "paused", "{paused}");

    fs::write(&fault_marker, "fail").unwrap();
    let ambiguous = call_tool(
        &mut server,
        4,
        "send_message",
        json!({
            "run_id": "run-message",
            "activation_id": "root",
            "message_id": "stable-message-1",
            "text": "message-boundary-payload"
        }),
    );
    assert_eq!(structured(&ambiguous)["ok"], false, "{ambiguous}");
    assert_eq!(
        structured(&ambiguous)["error"]["code"],
        "ambiguous_delivery"
    );
    let receipt = structured(&ambiguous)["receipt"].clone();
    assert_eq!(receipt["message_id"], "stable-message-1");
    assert_eq!(receipt["status"], "ambiguous_delivery");
    assert!(receipt["started_event_sequence"].as_u64().unwrap() > 0);

    let repeated = call_tool(
        &mut server,
        5,
        "send_message",
        json!({
            "run_id": "run-message",
            "activation_id": "root",
            "message_id": "stable-message-1",
            "text": "message-boundary-payload"
        }),
    );
    assert_eq!(structured(&repeated)["receipt"], receipt);
    assert_eq!(message_send_count(&root), 1);

    let newline_conflict = call_tool(
        &mut server,
        50,
        "send_message",
        json!({
            "run_id": "run-message",
            "activation_id": "root",
            "message_id": "stable-message-1",
            "text": "message-boundary-payload\n"
        }),
    );
    assert_eq!(
        structured(&newline_conflict)["ok"],
        false,
        "{newline_conflict}"
    );
    assert_eq!(
        structured(&newline_conflict)["error"]["code"],
        "message_id_conflict"
    );
    assert_eq!(message_send_count(&root), 1);
    assert!(
        receipt["payload_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    let run_root = store.run_root("run-message").unwrap();
    assert!(!run_root.join("machine-inputs.jsonl").exists());
    let ledger_records =
        fs::read_to_string(private_driver_dir(&root, &run_root).join("machine-inputs.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<MachineInputRecord>(line).unwrap())
            .collect::<Vec<_>>();
    let submitted_record = ledger_records
        .iter()
        .rev()
        .find(|record| record.normalized_text == "message-boundary-payload")
        .unwrap();
    assert_eq!(submitted_record.payload_hash, receipt["payload_hash"]);

    fs::remove_file(&fault_marker).unwrap();
    let resumed = call_tool(
        &mut server,
        6,
        "resume_run",
        json!({
            "run_id": "run-message",
            "delivery_resolution": {
                "started_event_sequence": receipt["started_event_sequence"],
                "outcome": "submitted",
                "evidence": "receiver-side transcript confirms the message"
            }
        }),
    );
    assert_eq!(structured(&resumed)["ok"], true, "{resumed}");
    assert_eq!(
        structured(&resumed)["delivery_resolution"]["outcome"],
        "submitted"
    );

    let submitted = call_tool(
        &mut server,
        7,
        "send_message",
        json!({
            "run_id": "run-message",
            "activation_id": "root",
            "message_id": "stable-message-1",
            "text": "message-boundary-payload"
        }),
    );
    assert_eq!(structured(&submitted)["ok"], true, "{submitted}");
    assert_eq!(structured(&submitted)["receipt"]["status"], "submitted");
    assert_eq!(
        structured(&submitted)["receipt"]["started_event_sequence"],
        receipt["started_event_sequence"]
    );
    assert_eq!(message_send_count(&root), 1);

    shutdown_driver(&run_root, "run-message");
    wait_for_driver_exit(&root);
    let restarted = call_tool(
        &mut server,
        8,
        "resume_run",
        json!({ "run_id": "run-message" }),
    );
    assert_eq!(structured(&restarted)["ok"], true, "{restarted}");
    let replayed = call_tool(
        &mut server,
        9,
        "send_message",
        json!({
            "run_id": "run-message",
            "activation_id": "root",
            "message_id": "stable-message-1",
            "text": "message-boundary-payload"
        }),
    );
    assert_eq!(structured(&replayed)["ok"], true, "{replayed}");
    assert_eq!(
        structured(&replayed)["receipt"],
        structured(&submitted)["receipt"]
    );
    let replayed_conflict = call_tool(
        &mut server,
        10,
        "send_message",
        json!({
            "run_id": "run-message",
            "activation_id": "root",
            "message_id": "stable-message-1",
            "text": "message-boundary-payload\n"
        }),
    );
    assert_eq!(
        structured(&replayed_conflict)["error"]["code"],
        "message_id_conflict",
        "{replayed_conflict}"
    );
    assert_eq!(message_send_count(&root), 1);

    shutdown_driver(&run_root, "run-message");
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);
    restore_env("HUMANIZE_STATE_ROOT", prior_state_root);
    restore_env("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", prior_fault);
    restore_env("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND", prior_kind);
}

fn agent_flow() -> serde_json::Value {
    json!({
        "nodes": [{
            "id": "root",
            "action": {
                "driver": "agent",
                "prompt_ref": "prompt.root",
                "resource_refs": ["README.md"]
            }
        }],
        "resources": [
            {
                "path": "README.md",
                "kind": "readme",
                "content": "Run the targeted participant message test."
            },
            {
                "path": "prompt.root",
                "kind": "prompt",
                "content": "Wait for targeted messages."
            }
        ]
    })
}

fn test_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir()
        .join("humanize-plugin-messages")
        .join(format!("{name}-{}", std::process::id()));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    root
}

fn message_send_count(root: &Path) -> usize {
    fs::read_to_string(root.join("tmux.log"))
        .unwrap()
        .matches("message-boundary-payload")
        .count()
}

fn shutdown_driver(run_root: &Path, run_id: &str) {
    if let Some(client) = DriverClient::from_run_root_for_run(run_root, run_id).unwrap() {
        let _ = client.request("shutdown", run_id, &json!({}));
    }
}

fn private_driver_dir(root: &Path, run_root: &Path) -> PathBuf {
    let identity = std::path::absolute(run_root)
        .unwrap_or_else(|_| run_root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    root.join("runtime")
        .join(format!("r{:016x}", stable_hash(&identity)))
        .join("driver")
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn wait_for_driver_exit(root: &Path) {
    let pid: i32 = fs::read_to_string(root.join("driver.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("driver process {pid} did not exit");
}

fn restore_env(key: &str, prior: Option<std::ffi::OsString>) {
    unsafe {
        match prior {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}
