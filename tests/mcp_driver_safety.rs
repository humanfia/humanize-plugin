mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use humanize_plugin::adapters::tmux::SystemCommandRunner;
use humanize_plugin::mcp::{McpServer, TmuxExecutionDefaults};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use support::driver_tmux::ControlledTmuxFixture;
use support::mcp::{call_tool, lock_flow, structured};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn bind_failure_external_cleanup_preserves_final_capture_and_manifest_completion() {
    let _guard = lock_test_environment();
    let root = test_root("bind-lifecycle");
    let tmux_control = ControlledTmuxFixture::new(&root);
    let fake_tmux = fake_tmux(&root);
    let fault_marker = root.join("fail-agent-launch-submitted");
    fs::write(&fault_marker, "fail").unwrap();
    let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
    let prior_fault = std::env::var_os("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS");
    let prior_kind = std::env::var_os("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND");
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
        std::env::set_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", &fault_marker);
        std::env::set_var(
            "HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND",
            "agent_launch_submitted",
        );
    }

    let mut server = server(&root, "humanize-test-agent");
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow("Inspect it."));
    let failed = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-bind-lifecycle",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(tmux_control.wait_for_hooks());
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);
    restore_env("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", prior_fault);
    restore_env("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND", prior_kind);

    assert_eq!(structured(&failed)["ok"], false, "{failed}");

    let run_root = run_root(&root, "run-bind-lifecycle");
    let driver_pid = fs::read_to_string(root.join("driver.pid"))
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    wait_until(Duration::from_secs(2), || unsafe {
        libc::kill(driver_pid as i32, 0) != 0
    });
    let tmux_log = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert_eq!(
        tmux_log
            .lines()
            .filter(|line| *line == "kill-pane -t host-a:%7.%9")
            .count(),
        1,
        "{tmux_log}"
    );
    assert_eq!(
        tmux_log
            .lines()
            .filter(|line| *line == "kill-pane -t host-a:%7.%8")
            .count(),
        1,
        "{tmux_log}"
    );
    assert_eq!(
        tmux_log
            .lines()
            .filter(|line| line.starts_with("split-window "))
            .count(),
        1,
        "bind cleanup must not retry node allocation: {tmux_log}"
    );
    let driver_dir = private_driver_dir_for_run_root(&run_root);
    let driver_events = fs::read_to_string(driver_dir.join("driver-events.jsonl")).unwrap();
    assert!(driver_events.contains("\"kind\":\"driver_pane_released\""));
    assert!(!driver_dir.join("ipc.json").exists());
    assert!(!driver_dir.join("ipc-token").exists());
    assert!(!run_root.join("driver").exists());

    let manifest: Value =
        serde_json::from_slice(&fs::read(driver_dir.join("run-assets.json")).unwrap()).unwrap();
    let activation = &manifest["activations"]["root"];
    assert_eq!(activation["capture_phase"], "complete");
    assert_eq!(activation["capture_complete"], true);
    assert_eq!(activation["resource_cleanup_status"], "complete");
    let transcript = activation["pipe_path"].as_str().unwrap();
    let final_capture = activation["final_capture_path"].as_str().unwrap();
    assert!(fs::metadata(transcript).unwrap().len() > 0);
    assert!(fs::metadata(final_capture).unwrap().len() > 0);
    assert!(
        fs::read_to_string(transcript)
            .unwrap()
            .contains("capture online")
    );
    assert!(
        fs::read_to_string(final_capture)
            .unwrap()
            .contains("final capture")
    );
}

#[test]
fn cleanup_intent_persistence_failure_reconciles_without_duplicate_physical_release() {
    let _guard = lock_test_environment();
    let root = test_root("release-event-reconciliation");
    let tmux_control = ControlledTmuxFixture::new(&root);
    let fake_tmux = fake_tmux(&root);
    let fault_marker = root.join("fail-release-events");
    let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
    let prior_fault = std::env::var_os("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS");
    let prior_kind = std::env::var_os("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND");
    let prior_after_agent = std::env::var_os("HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT");
    let prior_probe_fault = std::env::var_os("HUMANIZE_TEST_FAIL_LIST_PANES");
    let prior_timeout = std::env::var_os("HUMANIZE_DRIVER_READY_TIMEOUT_MS");
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
        std::env::set_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", &fault_marker);
        std::env::remove_var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND");
        std::env::set_var(
            "HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT",
            &fault_marker,
        );
        std::env::set_var("HUMANIZE_DRIVER_READY_TIMEOUT_MS", "400");
    }

    let run_id = "run-release-event-reconciliation";
    let mut server = server(&root, "humanize-test-agent");
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow("Inspect it."));
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
    assert!(tmux_control.wait_for_hooks());
    assert_eq!(structured(&failed)["ok"], false, "{failed}");
    let cleanup = &structured(&failed)["error"]["cleanup"];
    assert_eq!(cleanup["attempted"], 1, "{failed}");
    assert_eq!(cleanup["failed"], 0, "{failed}");
    assert_eq!(cleanup["status"], "complete", "{failed}");
    let public_failure = structured(&failed).to_string();
    assert!(!public_failure.contains("pane_id"), "{failed}");
    assert!(!public_failure.contains("%8"), "{failed}");
    assert!(!public_failure.contains("%9"), "{failed}");

    fs::remove_file(&fault_marker).unwrap();
    unsafe {
        std::env::remove_var("HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT");
        std::env::set_var("HUMANIZE_TEST_FAIL_LIST_PANES", "1");
    }
    let transient = call_tool(
        &mut server,
        3,
        "run_flow",
        json!({
            "run_id": run_id,
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(transient["error"].is_object(), "{transient}");
    let run_root = run_root(&root, run_id);
    let events_after_transient =
        fs::read_to_string(private_driver_dir_for_run_root(&run_root).join("driver-events.jsonl"))
            .unwrap();
    assert!(!events_after_transient.contains("\"kind\":\"driver_pane_released\""));
    assert!(!events_after_transient.contains("\"kind\":\"tmux_panes_released\""));

    unsafe {
        std::env::remove_var("HUMANIZE_TEST_FAIL_LIST_PANES");
    }
    let recovered = call_tool(
        &mut server,
        4,
        "run_flow",
        json!({
            "run_id": run_id,
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(tmux_control.wait_for_hooks());
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);
    restore_env("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS", prior_fault);
    restore_env("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND", prior_kind);
    restore_env(
        "HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT",
        prior_after_agent,
    );
    restore_env("HUMANIZE_TEST_FAIL_LIST_PANES", prior_probe_fault);
    restore_env("HUMANIZE_DRIVER_READY_TIMEOUT_MS", prior_timeout);

    assert_eq!(structured(&recovered)["ok"], true, "{recovered}");
    assert_eq!(structured(&recovered)["attached"], true, "{recovered}");
    assert_eq!(
        structured(&recovered)["run_status"],
        "paused",
        "{recovered}"
    );
    let context = call_tool(&mut server, 5, "get_context", json!({ "run_id": run_id }));
    let started_event_sequence =
        structured(&context)["context"]["ambiguous_deliveries"][0]["started_event_sequence"]
            .as_u64()
            .unwrap();
    let resumed = call_tool(
        &mut server,
        6,
        "resume_run",
        json!({
            "run_id": run_id,
            "delivery_resolution": {
                "started_event_sequence": started_event_sequence,
                "outcome": "submitted",
                "evidence": "the released pane received Enter before cleanup completed"
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
    let tmux_log = fs::read_to_string(root.join("tmux.log")).unwrap();
    assert!(tmux_log.contains("--driver-pane-id '%11'"), "{tmux_log}");
    assert_eq!(kill_count(&tmux_log, "%8"), 1, "{tmux_log}");
    assert_eq!(kill_count(&tmux_log, "%9"), 1, "{tmux_log}");
    assert_eq!(kill_count(&tmux_log, "%10"), 1, "{tmux_log}");

    let events =
        fs::read_to_string(private_driver_dir_for_run_root(&run_root).join("driver-events.jsonl"))
            .unwrap();
    assert!(events.contains("\"kind\":\"driver_pane_released\""));
    assert!(events.contains("\"kind\":\"tmux_panes_released\""));
    assert!(events.contains("\"pane_id\":\"%11\""));
    shutdown_driver(&run_root, run_id);
}

#[test]
fn agent_launch_failure_warning_does_not_expose_command_text() {
    let _guard = lock_test_environment();
    let root = test_root("agent-command-redaction");
    let tmux_control = ControlledTmuxFixture::new(&root);
    let fake_tmux = fake_tmux(&root);
    let secret = "AGENT_COMMAND_SECRET_7e91";
    let agent_command = format!("humanize-test-agent --credential {secret}");
    let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
    let prior_failure = std::env::var_os("HUMANIZE_TEST_FAIL_SEND_CONTAINS");
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
        std::env::set_var("HUMANIZE_TEST_FAIL_SEND_CONTAINS", secret);
    }

    let mut server = server(&root, &agent_command);
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow("Inspect it."));
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-agent-command-redaction",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(tmux_control.wait_for_hooks());
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);
    restore_env("HUMANIZE_TEST_FAIL_SEND_CONTAINS", prior_failure);

    assert_eq!(structured(&started)["ok"], true, "{started}");
    let warning = &structured(&started)["tmux"]["actuation"]["warnings"][0];
    assert_eq!(warning["role"], "agent_launch");
    assert_safe_tmux_warning(warning, secret);
    assert!(!structured(&started).to_string().contains(secret));

    shutdown_driver(
        &run_root(&root, "run-agent-command-redaction"),
        "run-agent-command-redaction",
    );
}

#[test]
fn node_prompt_failure_warning_does_not_expose_prompt_text() {
    let _guard = lock_test_environment();
    let root = test_root("node-prompt-redaction");
    let tmux_control = ControlledTmuxFixture::new(&root);
    let fake_tmux = fake_tmux(&root);
    let secret = "NODE_PROMPT_SECRET_4c20";
    let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
    let prior_failure = std::env::var_os("HUMANIZE_TEST_FAIL_SEND_CONTAINS");
    unsafe {
        std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
        std::env::set_var("HUMANIZE_TEST_FAIL_SEND_CONTAINS", secret);
    }

    let mut server = server(&root, "humanize-test-agent");
    let (lock_id, content_hash) = lock_flow(
        &mut server,
        1,
        locked_agent_flow(&format!("Inspect the repository. {secret}")),
    );
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": "run-node-prompt-redaction",
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
        }),
    );
    assert!(tmux_control.wait_for_hooks());
    restore_env("HUMANIZE_TMUX_BIN", prior_tmux);
    restore_env("HUMANIZE_TEST_FAIL_SEND_CONTAINS", prior_failure);

    assert_eq!(structured(&started)["ok"], true, "{started}");
    assert_eq!(
        structured(&started)["tmux"]["actuation"]["warnings"][0]["status"],
        "readiness_pending"
    );
    let status = call_tool(
        &mut server,
        3,
        "run_status",
        json!({ "run_id": "run-node-prompt-redaction" }),
    );
    assert_eq!(
        structured(&status)["context"]["run_status_reason"],
        "effect_reconciliation_required",
        "{status}"
    );
    assert_eq!(
        structured(&status)["context"]["ambiguous_deliveries"][0]["role"],
        "node_prompt",
        "{status}"
    );
    assert!(!status.to_string().contains(secret), "{status}");

    shutdown_driver(
        &run_root(&root, "run-node-prompt-redaction"),
        "run-node-prompt-redaction",
    );
}

fn assert_safe_tmux_warning(warning: &Value, secret: &str) {
    let rendered = warning.to_string();
    assert!(!rendered.contains(secret), "{warning}");
    let error = warning["error"].as_str().unwrap();
    assert!(error.contains("operation=send-keys"), "{error}");
    assert!(error.contains("command_hash=sha256:"), "{error}");
    assert!(error.contains("command_length="), "{error}");
}

fn server(root: &Path, agent_command: &str) -> McpServer<SystemCommandRunner> {
    McpServer::with_tmux_runner_run_asset_store_and_execution_defaults(
        SystemCommandRunner,
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs"))),
        TmuxExecutionDefaults {
            session: Some("host-a".into()),
            window: Some("flow-a".into()),
            agent_command: Some(agent_command.to_string()),
        },
    )
}

fn run_root(root: &Path, run_id: &str) -> PathBuf {
    RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")))
        .run_root(run_id)
        .unwrap()
}

fn locked_agent_flow(prompt: &str) -> Value {
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
                "content": "Inspect without editing files."
            },
            {
                "path": "prompt.start",
                "kind": "prompt",
                "content": prompt
            }
        ]
    })
}

fn test_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir()
        .join(format!("humanize-plugin-{name}"))
        .join(std::process::id().to_string());
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    unsafe {
        std::env::set_var("HUMANIZE_STATE_ROOT", &root);
    }
    root
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
fail_matching_input() {{
  input="$1"
  if test -n "$HUMANIZE_TEST_FAIL_SEND_CONTAINS"; then
    case "$input" in
      *"$HUMANIZE_TEST_FAIL_SEND_CONTAINS"*)
        printf 'tmux input failed: redacted\n' >&2
        exit 47
        ;;
    esac
  fi
}}
handle_input() {{
  input="$1"
  case "$input" in
    *--run-id*)
      TMUX_PANE="${{target##*.}}" sh -c "$input" >> "$root/driver.out" 2>> "$root/driver.err" &
      printf '%s\n' "$!" > "$root/driver.pid"
      ;;
    *humanize-test-agent*)
      if test -n "$HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT"; then
        : > "$HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT"
      fi
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
    if test -f "$root/pane.counter"; then
      pane="$(( $(cat "$root/pane.counter") + 1 ))"
    else
      pane='8'
    fi
    printf '%s\n' "$pane" > "$root/pane.counter"
    pane_id="%$pane"
    pane_target="host-a:%7.$pane_id"
    printf '%s\n' "$pane_target" >> "$root/live-panes"
    printf '%s\n' "$pane_id" >> "$root/driver-panes"
    printf '%s\t%s\n' '%7' "$pane_id"
    ;;
  split-window)
    pane="$(cat "$root/pane.counter")"
    pane="$((pane + 1))"
    printf '%s\n' "$pane" > "$root/pane.counter"
    pane_id="%$pane"
    printf '%s\n' "host-a:%7.$pane_id" >> "$root/live-panes"
    printf '%s\n' "$pane_id"
    ;;
  display-message)
    if test ! -f "$root/live-panes" || ! grep -Fx "$target" "$root/live-panes" >/dev/null 2>&1; then
      exit 42
    fi
    pane="${{target##*.}}"
    printf '%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' "$pane"
    ;;
  list-panes)
    if test -n "$HUMANIZE_TEST_FAIL_LIST_PANES"; then exit 49; fi
    if test -f "$root/live-panes"; then
      while IFS= read -r pane_target; do
        pane="${{pane_target##*.}}"
        printf '%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' "$pane"
      done < "$root/live-panes"
    fi
    ;;
  pipe-pane)
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" start "$root" "${{pane#%}}" "$last"
    ;;
  capture-pane)
    printf 'final capture for %s\n' "$target"
    ;;
  set-buffer)
    fail_matching_input "$last"
    printf '%s' "$last" > "$root/tmux-buffer-$buffer"
    ;;
  paste-buffer)
    input="$(cat "$root/tmux-buffer-$buffer")"
    rm -f "$root/tmux-buffer-$buffer"
    handle_input "$input"
    ;;
  send-keys)
    fail_matching_input "$last"
    handle_input "$last"
    ;;
  kill-pane)
    if test -f "$root/killed-panes" && grep -Fx "$target" "$root/killed-panes" >/dev/null 2>&1; then
      printf 'duplicate kill rejected for %s\n' "$target" >&2
      exit 48
    fi
    printf '%s\n' "$target" >> "$root/killed-panes"
    if test -f "$root/live-panes"; then
      grep -Fvx "$target" "$root/live-panes" > "$root/live-panes.tmp"
      mv "$root/live-panes.tmp" "$root/live-panes"
    fi
    pane="${{target##*.}}"
    if test -f "$root/driver-panes" && grep -Fx "$pane" "$root/driver-panes" >/dev/null 2>&1; then
      if test -f "$root/driver.pid"; then
        kill "$(cat "$root/driver.pid")" 2>/dev/null || true
      fi
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

fn shutdown_driver(run_root: &Path, run_id: &str) {
    let driver_dir = private_driver_dir_for_run_root(run_root);
    let metadata: Value =
        serde_json::from_slice(&fs::read(driver_dir.join("ipc.json")).unwrap()).unwrap();
    let token =
        fs::read_to_string(driver_dir.join(metadata["auth_token_path"].as_str().unwrap())).unwrap();
    let socket_path = driver_dir
        .parent()
        .unwrap()
        .join(metadata["socket_path"].as_str().unwrap());
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
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], true, "{response}");
}

fn private_driver_dir_for_run_root(run_root: &Path) -> PathBuf {
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

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
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

fn wait_until(timeout: Duration, condition: impl Fn() -> bool) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < timeout, "condition did not become true");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn kill_count(log: &str, pane_id: &str) -> usize {
    log.lines()
        .filter(|line| line == &format!("kill-pane -t host-a:%7.{pane_id}"))
        .count()
}
