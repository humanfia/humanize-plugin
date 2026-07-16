use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use super::driver_flows::{later_agent_flow, routed_locked_flow};
use super::support::DriverFixture;

#[test]
fn driver_owns_tmux_node_panes_for_initial_and_routed_activations() {
    let fixture = DriverFixture::new("driver-tmux-owned");
    let fake_tmux = fixture.fake_tmux_without_agent_ready();
    let mut driver = fixture.spawn_with_env("run-tmux-owned", &[("HUMANIZE_TMUX_BIN", &fake_tmux)]);

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-tmux-owned",
        "flow_lock": routed_locked_flow(),
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        },
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);
    assert_eq!(bound["tmux"]["panes"][0]["activation_id"], "root");
    assert_eq!(bound["tmux"]["panes"].as_array().unwrap().len(), 1);

    let delivered = fixture.request(json!({
        "id": "deliver-brief",
        "token": fixture.token,
        "op": "deliver_artifact",
        "run_id": "run-tmux-owned",
        "activation_id": "root",
        "artifact_id": "brief",
        "payload": "ready"
    }));
    assert_eq!(delivered["ok"], true);
    assert_eq!(delivered["tmux_allocations"][0]["activation_id"], "follow");

    let calls = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert!(calls.contains("has-session -t host-a"));
    assert!(calls.contains("new-session -d -P -F #{window_id}|#{pane_id} -s host-a -n flow-a"));
    assert!(calls.contains("split-window -P -F #{pane_id} -t host-a:%7 -v"));
    driver.shutdown();
}

#[test]
fn driver_activate_node_allocates_and_actuates_missing_agent_pane() {
    let fixture = DriverFixture::new("driver-activate-actuates");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-activate-agent")
        .unwrap();
    let fake_tmux = fixture.fake_tmux_with_agent_ready();
    let mut driver =
        fixture.spawn_with_env("run-activate-agent", &[("HUMANIZE_TMUX_BIN", &fake_tmux)]);

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-activate-agent",
        "flow_lock": later_agent_flow(),
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        },
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);
    assert_eq!(bound["tmux"]["panes"][0]["activation_id"], "root");

    let activated = fixture.request(json!({
        "id": "activate-manual",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-activate-agent",
        "node_id": "manual"
    }));
    assert_eq!(activated["ok"], true, "{activated}");
    assert_eq!(activated["activation_id"], "manual");
    assert_eq!(activated["tmux_allocations"][0]["activation_id"], "manual");
    assert_eq!(
        activated["actuation"]["warnings"][0]["status"],
        "readiness_pending"
    );
    wait_for(Duration::from_secs(2), || {
        fs::read_to_string(fixture.tmux_log())
            .is_ok_and(|calls| calls.contains("Inspect the manual node."))
    });
    wait_for(Duration::from_secs(2), || {
        fs::read_to_string(fixture.driver_events_path("run-activate-agent"))
            .is_ok_and(|events| events.contains("\"kind\":\"prompt_submitted\""))
    });

    let calls = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert!(calls.contains("split-window -P -F #{pane_id} -t host-a:%7 -v"));
    assert!(calls.lines().any(|call| {
        call.starts_with("set-buffer -b ")
            && call.contains(" -- env HUMANIZE_PARTICIPANT_BINDING_FILE=")
    }));
    assert!(!calls.contains("HUMANIZE_PARTICIPANT_CREDENTIAL="));
    assert!(calls.contains("humanize-test-agent"));
    assert!(calls.contains("send-keys -t host-a:%7.%9 C-u"));
    assert!(calls.contains("send-keys -t host-a:%7.%9 Enter"));
    assert!(calls.contains("Inspect the manual node."));
    let driver_events_path = fixture.driver_events_path("run-activate-agent");
    let driver_events = fs::read_to_string(&driver_events_path).unwrap();
    assert!(driver_events.contains("\"kind\":\"agent_launch_submitted\""));
    assert!(driver_events.contains("\"kind\":\"prompt_submitted\""));
    assert!(!driver_events.contains("\"kind\":\"agent_launched\""));
    assert!(!driver_events.contains("\"kind\":\"node_prompt_sent\""));
    driver.crash();

    let legacy_events = driver_events
        .lines()
        .map(|line| {
            let mut event: Value = serde_json::from_str(line).unwrap();
            match event["kind"].as_str() {
                Some("agent_launch_submitted") => {
                    event["kind"] = Value::String("agent_launched".into());
                    event["payload"]
                        .as_object_mut()
                        .unwrap()
                        .remove("started_event_sequence");
                }
                Some("prompt_submitted") => {
                    event["kind"] = Value::String("node_prompt_sent".into());
                    event["payload"]
                        .as_object_mut()
                        .unwrap()
                        .remove("started_event_sequence");
                }
                _ => {}
            }
            serde_json::to_string(&event).unwrap()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&driver_events_path, legacy_events).unwrap();
    let calls_before_restart = fs::read_to_string(fixture.tmux_log()).unwrap();
    let mut restarted =
        fixture.spawn_with_env("run-activate-agent", &[("HUMANIZE_TMUX_BIN", &fake_tmux)]);
    let resumed = fixture.request(json!({
        "id": "resume-legacy-submission-events",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-activate-agent"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    let input_before = calls_before_restart
        .lines()
        .filter(|line| {
            line.starts_with("set-buffer ")
                || line.starts_with("paste-buffer ")
                || line.starts_with("send-keys ")
        })
        .collect::<Vec<_>>();
    let calls_after_restart = fs::read_to_string(fixture.tmux_log()).unwrap();
    let input_after = calls_after_restart
        .lines()
        .filter(|line| {
            line.starts_with("set-buffer ")
                || line.starts_with("paste-buffer ")
                || line.starts_with("send-keys ")
        })
        .collect::<Vec<_>>();
    assert_eq!(input_after, input_before);
    restarted.shutdown();
}

#[test]
fn driver_resume_retries_prompt_after_agent_readiness_warning() {
    let fixture = DriverFixture::new("driver-resume-actuates");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-resume-agent")
        .unwrap();
    let fake_tmux = fixture.fake_tmux_without_agent_ready();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn_with_env_values(
        "run-resume-agent",
        &[
            ("HUMANIZE_TMUX_BIN", &fake_tmux_value),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
        ],
    );

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-resume-agent",
        "flow_lock": later_agent_flow(),
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent"
        },
        "review": {
            "review_id": "review-approved",
            "status": "approved"
        }
    }));
    assert_eq!(bound["ok"], true);
    let activated = fixture.request(json!({
        "id": "activate-manual",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-resume-agent",
        "node_id": "manual"
    }));
    assert_eq!(activated["ok"], true, "{activated}");
    assert_eq!(
        activated["actuation"]["sent"].as_array().unwrap().len(),
        0,
        "{activated}"
    );
    assert_eq!(
        activated["actuation"]["warnings"][0]["activation_id"],
        "manual"
    );
    let paused = fixture.request(json!({
        "id": "pause-before-readiness-retry",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-resume-agent"
    }));
    assert_eq!(paused["ok"], true, "{paused}");
    send_session_start(
        &participant_binding(&fixture, "run-resume-agent", "manual"),
        "%9",
        "native-resume-session",
    );
    let before_resume = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert!(!before_resume.contains("Inspect the manual node."));

    let resumed = fixture.request(json!({
        "id": "resume-run",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-resume-agent"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(resumed["tmux_allocations"].as_array().unwrap().len(), 0);
    assert_eq!(resumed["actuation"]["sent"][0]["activation_id"], "manual");
    let calls = fs::read_to_string(fixture.tmux_log()).unwrap();
    assert_eq!(
        calls
            .matches("split-window -P -F #{pane_id} -t host-a:%7 -v")
            .count(),
        1
    );
    assert!(calls.contains("send-keys -t host-a:%7.%9 C-u"));
    assert!(calls.contains("Inspect the manual node."));
    driver.shutdown();
}

#[test]
fn driver_replays_tmux_prompt_actuation_config_after_restart() {
    let fixture = DriverFixture::new("driver-replay-prompt-actuation");
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(fixture.root.join("runs")))
        .start_run_manifest("run-replay-prompt-actuation")
        .unwrap();
    let fake_tmux = fixture.fake_tmux_without_agent_ready();
    let mut driver = fixture.spawn_with_env(
        "run-replay-prompt-actuation",
        &[("HUMANIZE_TMUX_BIN", &fake_tmux)],
    );

    let bound = fixture.request(json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-replay-prompt-actuation",
        "flow_lock": later_agent_flow(),
        "tmux": {
            "enabled": true,
            "session": "host-a",
            "window": "flow-a",
            "agent_command": "humanize-test-agent",
            "agent_ready_pattern": "final capture for",
            "agent_ready_timeout_ms": 100,
            "prompt_submit_key_count": 2
        }
    }));
    assert_eq!(bound["ok"], true, "{bound}");

    let activated = fixture.request(json!({
        "id": "activate-manual",
        "token": fixture.token,
        "op": "activate",
        "run_id": "run-replay-prompt-actuation",
        "node_id": "manual"
    }));
    assert_eq!(activated["ok"], true, "{activated}");
    assert_eq!(
        activated["actuation"]["warnings"][0]["status"],
        "readiness_pending"
    );
    let paused = fixture.request(json!({
        "id": "pause-before-restart",
        "token": fixture.token,
        "op": "pause",
        "run_id": "run-replay-prompt-actuation"
    }));
    assert_eq!(paused["ok"], true, "{paused}");
    send_session_start(
        &participant_binding(&fixture, "run-replay-prompt-actuation", "manual"),
        "%9",
        "native-replay-session",
    );

    let driver_events =
        fs::read_to_string(fixture.driver_events_path("run-replay-prompt-actuation")).unwrap();
    assert!(driver_events.contains("\"prompt_submit_key_count\":2"));
    assert!(driver_events.contains("\"agent_ready_pattern\":\"final capture for\""));
    let calls_before_restart = fs::read_to_string(fixture.tmux_log()).unwrap();
    let prior_call_count = calls_before_restart.lines().count();
    driver.crash();

    let mut restarted = fixture.spawn_with_env(
        "run-replay-prompt-actuation",
        &[("HUMANIZE_TMUX_BIN", &fake_tmux)],
    );
    let resumed = fixture.request(json!({
        "id": "resume-after-restart",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-replay-prompt-actuation"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(
        resumed["actuation"]["sent"][0]["prompt_submit_key_count"],
        2
    );
    assert_eq!(
        resumed["actuation"]["sent"][0]["readiness"]["tmux_marker"],
        "observed"
    );

    let calls = fs::read_to_string(fixture.tmux_log()).unwrap();
    let replay_calls = calls.lines().skip(prior_call_count).collect::<Vec<_>>();
    assert!(
        replay_calls
            .iter()
            .any(|call| call.contains("capture-pane -p -t host-a:%7.%9"))
    );
    assert!(
        replay_calls
            .iter()
            .any(|call| call.starts_with("set-buffer "))
    );
    assert!(
        replay_calls
            .iter()
            .any(|call| call.starts_with("paste-buffer "))
    );
    assert_eq!(
        replay_calls
            .iter()
            .filter(|call| **call == "send-keys -t host-a:%7.%9 Enter")
            .count(),
        2
    );
    restarted.shutdown();
}

fn send_session_start(binding: &Value, pane_id: &str, native_session_id: &str) {
    let mut hook = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .env("TMUX_PANE", pane_id)
        .env(
            "HUMANIZE_PARTICIPANT_BINDING_FILE",
            binding["binding_file_path"].as_str().unwrap(),
        )
        .arg("--agent-ready-hook")
        .arg("--source")
        .arg("codex_session_start")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        hook.stdin.as_mut().unwrap(),
        "{}",
        json!({"hook_event_name":"SessionStart","session_id":native_session_id})
    )
    .unwrap();
    let hook = hook.wait_with_output().unwrap();
    assert!(hook.status.success());
}

fn participant_binding(fixture: &DriverFixture, run_id: &str, activation_id: &str) -> Value {
    let mut binding = fs::read_to_string(fixture.driver_events_path(run_id))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| {
            event["kind"] == "participant_started"
                && event["payload"]["binding"]["activation_id"] == activation_id
        })
        .map(|event| event["payload"]["binding"].clone())
        .unwrap();
    let path = fixture.private_run_root(run_id).join(
        binding["binding_file"]
            .as_str()
            .expect("participant binding event must reference its private file"),
    );
    binding["binding_file_path"] = Value::String(path.to_string_lossy().into_owned());
    binding
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
