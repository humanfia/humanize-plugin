use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use humanize_plugin::run_assets::{
    RunAssetActivationUpdate, RunAssetSink, RunAssetStore, RunAssetTmuxTarget,
};
use serde_json::{Value, json};

const COMMAND: &str = "/opt/humanize-plugin-mcp";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

fn run_plugin(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .args(args)
        .output()
        .unwrap()
}

fn run_plugin_with_env(args: &[&str], envs: &[(&str, &Path)]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"));
    command.args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    if !envs.iter().any(|(name, _)| *name == "HUMANIZE_STATE_ROOT")
        && let Some((_, runs_root)) = envs.iter().find(|(name, _)| *name == "HUMANIZE_RUNS_DIR")
    {
        command.env("HUMANIZE_STATE_ROOT", runs_root.join("state"));
    }
    command.output().unwrap()
}

fn run_plugin_with_stdin_env(
    args: &[&str],
    input: &str,
    envs: &[(&str, &str)],
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"));
    command.args(args).envs(envs.iter().copied());
    if !envs.iter().any(|(name, _)| *name == "HUMANIZE_STATE_ROOT")
        && let Some((_, runs_root)) = envs.iter().find(|(name, _)| *name == "HUMANIZE_RUNS_DIR")
    {
        command.env("HUMANIZE_STATE_ROOT", Path::new(runs_root).join("state"));
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn run_plugin_with_stdin_path_env(
    args: &[&str],
    stdin_path: &Path,
    envs: &[(&str, &str)],
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"));
    command.args(args).envs(envs.iter().copied());
    if !envs.iter().any(|(name, _)| *name == "HUMANIZE_STATE_ROOT")
        && let Some((_, runs_root)) = envs.iter().find(|(name, _)| *name == "HUMANIZE_RUNS_DIR")
    {
        command.env("HUMANIZE_STATE_ROOT", Path::new(runs_root).join("state"));
    }
    command
        .stdin(Stdio::from(fs::File::open(stdin_path).unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn temp_root(name: &str) -> PathBuf {
    let index = NEXT_TEMP.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("humanize-{name}-{}-{index}", std::process::id()));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    root
}

struct OwnedPaneFixture {
    readiness_nonce: String,
    requests_path: PathBuf,
    reject_marker: PathBuf,
    socket_path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OwnedPaneFixture {
    fn requests(&self) -> Vec<Value> {
        fs::read_to_string(&self.requests_path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn reject_guard_requests(&self) {
        fs::write(&self.reject_marker, "reject\n").unwrap();
    }
}

impl Drop for OwnedPaneFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket_path);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn create_owned_pane_manifest(root: &Path, run_id: &str, activation_id: &str) -> OwnedPaneFixture {
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.to_path_buf()));
    let mut manifest = store.start_run_manifest(run_id).unwrap();
    store
        .start_activation_capture(
            &mut manifest,
            RunAssetActivationUpdate {
                activation_id: activation_id.to_string(),
                node_id: activation_id.to_string(),
                adapter: "tmux".to_string(),
                tmux: RunAssetTmuxTarget {
                    session_id: "host-a".into(),
                    window_id: "%7".into(),
                    window_name: "flow-a".into(),
                    pane_id: "%8".into(),
                    allocation_generation: 0,
                },
                termination_reason: None,
            },
        )
        .unwrap();
    let readiness_nonce = manifest.activations[activation_id].readiness_nonce.clone();
    let state_root = root.join("state");
    let runtime_root = state_root.join("runtime");
    let private_run_root = private_run_root(&runtime_root, &manifest.root);
    let driver_root = private_run_root.join("driver");
    for path in [&state_root, &runtime_root, &private_run_root, &driver_root] {
        fs::create_dir_all(path).unwrap();
        set_mode(path, 0o700);
    }
    write_private_json(
        &private_run_root.join("identity.json"),
        &json!({
            "schema": "humanize.private_run_identity.v1",
            "run_id": run_id,
            "public_run_root": std::path::absolute(&manifest.root).unwrap(),
            "runs_root": std::path::absolute(root).unwrap()
        }),
    );
    write_private_jsonl(
        &driver_root.join("driver-events.jsonl"),
        &json!({
            "seq": 1,
            "at_ms": 1,
            "kind": "tmux_pane_allocated",
            "payload": {
                "activation_id": activation_id,
                "pane": {
                    "session_id": "host-a",
                    "window_id": "%7",
                    "window_name": "flow-a",
                    "pane_id": "%8",
                    "allocation_generation": 0
                }
            }
        }),
    );

    let token = "guard-test-token";
    write_private(
        &driver_root.join("ipc-token"),
        format!("{token}\n").as_bytes(),
    );
    let socket_path = private_run_root.join("s");
    let listener = UnixListener::bind(&socket_path).unwrap();
    set_mode(&socket_path, 0o600);
    listener.set_nonblocking(true).unwrap();
    write_private_json(
        &driver_root.join("ipc.json"),
        &json!({
            "run_id": run_id,
            "socket_path": "s",
            "auth_token_path": "ipc-token",
            "updated_at_ms": 1
        }),
    );
    let requests_path = driver_root.join("guard-requests.jsonl");
    let reject_marker = driver_root.join("reject-guard");
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread_requests = requests_path.clone();
    let thread_reject = reject_marker.clone();
    let thread = thread::spawn(move || {
        while !thread_stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    respond_to_guard_request(stream, &thread_requests, &thread_reject, token)
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(err) => panic!("guard fixture accept failed: {err}"),
            }
        }
    });

    OwnedPaneFixture {
        readiness_nonce,
        requests_path,
        reject_marker,
        socket_path,
        stop,
        thread: Some(thread),
    }
}

fn read_hook_records(root: &Path, run_id: &str) -> Vec<Value> {
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.to_path_buf()))
        .run_root(run_id)
        .unwrap();
    let requests_path = private_run_root(&root.join("state/runtime"), &run_root)
        .join("driver")
        .join("guard-requests.jsonl");
    fs::read_to_string(requests_path)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let request = serde_json::from_str::<Value>(line).unwrap();
            if request["op"] != "record_hook_fact" {
                return None;
            }
            let mut payload = request["payload"].clone();
            payload["target_run_id"] = request["run_id"].clone();
            payload["target_activation_id"] = request["activation_id"].clone();
            Some(json!({
                "fact": {
                    "hook": request["hook"],
                    "payload": payload
                }
            }))
        })
        .collect()
}

fn respond_to_guard_request(
    mut stream: UnixStream,
    requests_path: &Path,
    reject_marker: &Path,
    token: &str,
) {
    let mut line = String::new();
    if BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut line)
        .is_err()
        || line.trim().is_empty()
    {
        return;
    }
    let mut request = serde_json::from_str::<Value>(&line).unwrap();
    let authorized = request["token"] == token;
    if let Some(object) = request.as_object_mut() {
        object.remove("id");
        object.remove("token");
    }
    if authorized && request["op"] == "record_hook_fact" {
        let mut options = OpenOptions::new();
        options.create(true).append(true).mode(0o600);
        let mut file = options.open(requests_path).unwrap();
        writeln!(file, "{request}").unwrap();
        file.sync_all().unwrap();
    }
    let response = if authorized && !reject_marker.exists() {
        json!({"ok": true, "run_id": request["run_id"]})
    } else {
        json!({
            "ok": false,
            "error": {"code": "publication_blocked", "message": "guard evidence rejected"}
        })
    };
    writeln!(stream, "{response}").unwrap();
}

fn private_run_root(runtime_root: &Path, run_root: &Path) -> PathBuf {
    let identity = std::path::absolute(run_root)
        .unwrap_or_else(|_| run_root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    runtime_root.join(format!("r{:016x}", stable_hash(&identity)))
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn write_private_json(path: &Path, value: &Value) {
    let mut bytes = serde_json::to_vec(value).unwrap();
    bytes.push(b'\n');
    write_private(path, &bytes);
}

fn write_private_jsonl(path: &Path, value: &Value) {
    write_private_json(path, value);
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    set_mode(path, 0o600);
}

fn set_mode(path: &Path, mode: u32) {
    let mut permissions = fs::symlink_metadata(path).unwrap().permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn cli_prints_codex_session_snippet() {
    let output = run_plugin(&[
        "--print-client-config",
        "codex-session",
        "--command",
        COMMAND,
    ]);

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!(
            "codex -C \"$PWD\" \\\n",
            "  -c 'mcp_servers.humanize_plugin.command=\"/opt/humanize-plugin-mcp\"' \\\n",
            "  -c 'mcp_servers.humanize_plugin.args=[]'\n"
        )
    );
}

#[test]
fn cli_prints_parseable_claude_session_json() {
    let output = run_plugin(&[
        "--print-client-config",
        "claude-session-json",
        "--command",
        COMMAND,
    ]);

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        parsed["mcpServers"]["humanize_plugin"]["command"],
        json!(COMMAND)
    );
    assert_eq!(parsed["mcpServers"]["humanize_plugin"]["args"], json!([]));
}

#[test]
fn cli_rejects_unknown_target() {
    let output = run_plugin(&[
        "--print-client-config",
        "unknown-target",
        "--command",
        COMMAND,
    ]);

    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown client config target"));
    assert!(stderr.contains("usage: humanize-plugin-mcp"));
}

#[test]
fn cli_guard_blocks_owned_tmux_send() {
    let output = run_plugin(&[
        "--guard-tmux-send",
        "--owned-pane",
        "host-a:%7.%8",
        "--",
        "send-keys",
        "-t",
        "host-a:%7.%8",
        "-l",
        "inspect the repo",
    ]);

    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Use Humanize MCP"));
    assert!(stderr.contains("Humanize input tool"));
}

#[test]
fn cli_guard_allows_unowned_tmux_send() {
    let output = run_plugin(&[
        "--guard-tmux-send",
        "--owned-pane",
        "host-a:%7.%8",
        "--",
        "send-keys",
        "-t",
        "other:%1.%2",
        "Enter",
    ]);

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

#[test]
fn cli_guard_blocks_current_owned_pane_when_tmux_send_has_no_target() {
    let output = run_plugin(&[
        "--guard-tmux-send",
        "--owned-pane",
        "host-a:%7.%8",
        "--current-pane",
        "%8",
        "--",
        "send-keys",
        "-l",
        "inspect the repo",
    ]);

    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Direct tmux send to Humanize-owned pane %8 is blocked")
    );
}

#[test]
fn cli_guard_discovers_owned_panes_from_humanize_runs_dir() {
    let root = temp_root("cli-guard-discover");
    let _owned = create_owned_pane_manifest(&root, "run-owned", "root");

    let output = run_plugin_with_env(
        &[
            "--guard-tmux-send",
            "--",
            "send-keys",
            "-t",
            "host-a:%7.%8",
            "-l",
            "repair the prompt",
        ],
        &[("HUMANIZE_RUNS_DIR", &root)],
    );

    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Use Humanize MCP"));
    assert!(!stderr.contains("repair the prompt"));

    let allowed = run_plugin_with_env(
        &[
            "--guard-tmux-send",
            "--",
            "capture-pane",
            "-t",
            "host-a:%7.%8",
        ],
        &[("HUMANIZE_RUNS_DIR", &root)],
    );

    assert!(allowed.status.success());
}

#[test]
fn native_codex_and_claude_hooks_deny_owned_tmux_send_json() {
    let root = temp_root("cli-native-hook-deny");
    let _owned = create_owned_pane_manifest(&root, "run-hook", "root");
    let input = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {
            "command": "tmux send-keys -t host-a:%7.%8 -l 'secret prompt text'"
        }
    })
    .to_string();

    for flag in ["--codex-pre-tool-use-hook", "--claude-pre-tool-use-hook"] {
        let output = run_plugin_with_stdin_env(
            &[flag],
            &input,
            &[
                ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
                ("TMUX_PANE", "%1"),
            ],
        );

        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stderr), "");
        let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            decision["hookSpecificOutput"]["hookEventName"],
            "PreToolUse"
        );
        assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(
            decision["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("Use Humanize MCP")
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("secret prompt text"));
    }
}

#[test]
fn native_hooks_inspect_compound_commands_and_fail_closed_on_parse_errors() {
    let root = temp_root("cli-native-hook-compound");
    let _owned = create_owned_pane_manifest(&root, "run-hook-compound", "root");
    let commands = [
        "true && tmux send-keys -t host-a:%7.%8 Enter",
        "printf ready | tmux send-keys -t host-a:%7.%8 -l payload",
        "tmux send-keys -lt host-a:%7.%8 payload",
        "exec -a humanize-tmux tmux send-keys -t host-a:%7.%8 Enter",
        "env -i -u HOME PATH=/usr/bin tmux send-keys -t host-a:%7.%8 Enter",
        "eval 'tmux send-keys -lt host-a:%7.%8 eval-secret'",
        "eval 'eval \"tmux send-keys -t host-a:%7.%8 Enter\"'",
        "eval \"$TMUX_COMMAND\"",
        "echo 'unterminated",
    ];

    for flag in ["--codex-pre-tool-use-hook", "--claude-pre-tool-use-hook"] {
        for command in commands {
            let input = json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": { "command": command }
            });
            let output = run_plugin_with_stdin_env(
                &[flag],
                &input.to_string(),
                &[
                    ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
                    ("TMUX_PANE", "%1"),
                ],
            );

            assert_eq!(output.status.code(), Some(0), "{flag}: {command}");
            assert_eq!(String::from_utf8_lossy(&output.stderr), "");
            let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
        }
    }
}

#[test]
fn native_hooks_persist_normalized_evidence_without_literal_tmux_payloads() {
    let root = temp_root("cli-native-hook-evidence");
    let owned = create_owned_pane_manifest(&root, "run-hook-evidence", "root");
    let attempts = [
        (
            "clustered secret alpha",
            "tmux send-keys -lt host-a:%7.%8 'clustered secret alpha'",
        ),
        (
            "exec secret beta",
            "exec tmux send-keys -lt host-a:%7.%8 'exec secret beta'",
        ),
        (
            "env secret gamma",
            "env -i PATH=/usr/bin tmux send-keys -l -t host-a:%7.%8 'env secret gamma'",
        ),
        (
            "key secret delta",
            "tmux send-keys -t host-a:%7.%8 'key secret delta'",
        ),
    ];

    for (_, command) in attempts {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": command }
        });
        let output = run_plugin_with_stdin_env(
            &["--codex-pre-tool-use-hook"],
            &input.to_string(),
            &[
                ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
                ("TMUX_PANE", "%1"),
            ],
        );

        assert_eq!(output.status.code(), Some(0), "{command}");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "");
        let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
    }

    let records = read_hook_records(&root, "run-hook-evidence");
    assert_eq!(owned.requests().len(), attempts.len());
    let blocked = records
        .iter()
        .filter(|record| record["fact"]["hook"] == "humanize.tmux_guard_blocked")
        .collect::<Vec<_>>();
    assert_eq!(blocked.len(), attempts.len());
    let rendered = serde_json::to_string(&blocked).unwrap();
    for (secret, _) in attempts {
        assert!(
            !rendered.contains(secret),
            "persisted literal payload: {secret}"
        );
    }

    let mut payload_lengths = Vec::new();
    for record in blocked {
        let payload = &record["fact"]["payload"];
        assert_eq!(payload["decision"], "blocked");
        assert_eq!(payload["target_run_id"], "run-hook-evidence");
        assert_eq!(payload["target_activation_id"], "root");
        assert_eq!(payload["operation"], "send-keys");
        assert!(
            payload["option_flags"] == json!(["-l", "-t"])
                || payload["option_flags"] == json!(["-t"])
        );
        assert!(
            payload["target_hash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert!(
            payload["payload_hash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        payload_lengths.push(payload["payload_length"].as_u64().unwrap());
        assert!(payload.get("argv").is_none());
        assert!(payload.get("argv_hash").is_none());
        assert!(payload.get("target_tmux").is_none());
        assert!(payload.get("target_pane").is_none());
        assert!(payload.get("source_pane").is_none());
        assert!(!payload.to_string().contains("host-a:%7.%8"));
    }
    payload_lengths.sort_unstable();
    let mut expected_lengths = attempts
        .iter()
        .map(|(secret, _)| secret.len() as u64)
        .collect::<Vec<_>>();
    expected_lengths.sort_unstable();
    assert_eq!(payload_lengths, expected_lengths);
}

#[test]
fn native_hooks_fail_closed_on_protocol_errors() {
    let root = temp_root("cli-native-hook-protocol-errors");
    let cases = [
        (
            "malformed protocol secret",
            "{\"malformed protocol secret\":".to_string(),
        ),
        (
            "missing command secret",
            json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": { "note": "missing command secret" }
            })
            .to_string(),
        ),
        (
            "non-string command secret",
            json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": {
                    "command": 7,
                    "note": "non-string command secret"
                }
            })
            .to_string(),
        ),
        (
            "unsupported event secret",
            json!({
                "hook_event_name": "PostToolUse",
                "tool_name": "Bash",
                "tool_input": {
                    "command": "printf unsupported",
                    "note": "unsupported event secret"
                }
            })
            .to_string(),
        ),
        (
            "unsupported tool secret",
            json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Read",
                "tool_input": {
                    "command": "printf unsupported",
                    "note": "unsupported tool secret"
                }
            })
            .to_string(),
        ),
        (
            "non-object event secret",
            json!(["non-object event secret"]).to_string(),
        ),
        (
            "non-object tool input secret",
            json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": "non-object tool input secret"
            })
            .to_string(),
        ),
    ];

    for flag in ["--codex-pre-tool-use-hook", "--claude-pre-tool-use-hook"] {
        for (secret, input) in &cases {
            let output = run_plugin_with_stdin_env(
                &[flag],
                input,
                &[("HUMANIZE_RUNS_DIR", root.to_str().unwrap())],
            );

            assert_eq!(output.status.code(), Some(0), "{flag}: {secret}");
            assert_eq!(String::from_utf8_lossy(&output.stderr), "");
            let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                decision["hookSpecificOutput"]["hookEventName"],
                "PreToolUse"
            );
            assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
            assert!(
                decision["hookSpecificOutput"]["permissionDecisionReason"]
                    .as_str()
                    .unwrap()
                    .contains("protocol")
            );
            assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
        }
    }
}

#[test]
fn native_hooks_fail_closed_when_stdin_cannot_be_read() {
    let root = temp_root("cli-native-hook-read-error-secret");

    for flag in ["--codex-pre-tool-use-hook", "--claude-pre-tool-use-hook"] {
        let output = run_plugin_with_stdin_path_env(
            &[flag],
            &root,
            &[("HUMANIZE_RUNS_DIR", root.to_str().unwrap())],
        );

        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "");
        let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(
            decision["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("protocol")
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("read-error-secret"));
    }
}

#[test]
fn native_hooks_block_when_blocked_attempt_persistence_fails() {
    let root = temp_root("cli-native-hook-persistence-failure");
    let owned = create_owned_pane_manifest(&root, "run-hook-persistence-failure", "root");
    owned.reject_guard_requests();
    let input = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {
            "command": "tmux send-keys -t host-a:%7.%8 -l 'secret prompt text'"
        }
    })
    .to_string();

    for flag in ["--codex-pre-tool-use-hook", "--claude-pre-tool-use-hook"] {
        let output = run_plugin_with_stdin_env(
            &[flag],
            &input,
            &[
                ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
                ("TMUX_PANE", "%1"),
            ],
        );

        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "");
        let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(
            decision["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("could not be durably recorded")
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("secret prompt text"));
    }
}

#[test]
fn native_hooks_allow_unowned_tmux_send_and_non_send_commands() {
    let root = temp_root("cli-native-hook-allow");
    let _owned = create_owned_pane_manifest(&root, "run-hook-allow", "root");
    let inputs = [
        json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {
                "command": "tmux send-keys -t other:%1.%2 Enter"
            }
        }),
        json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {
                "command": "tmux capture-pane -t host-a:%7.%8"
            }
        }),
    ];

    for input in inputs {
        let output = run_plugin_with_stdin_env(
            &["--codex-pre-tool-use-hook"],
            &input.to_string(),
            &[
                ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
                ("TMUX_PANE", "%1"),
            ],
        );

        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    }
}

#[test]
fn guarded_tmux_wrapper_passes_allowed_and_records_blocked_without_prompt_text() {
    let root = temp_root("cli-guarded-wrapper");
    let _owned = create_owned_pane_manifest(&root, "run-wrapper", "root");
    let fake_tmux = root.join("real-tmux");
    let calls = root.join("tmux-calls.txt");
    fs::write(
        &fake_tmux,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
            calls.display()
        ),
    )
    .unwrap();
    make_executable(&fake_tmux);

    let allowed = run_plugin_with_stdin_env(
        &["--guarded-tmux", "--", "capture-pane", "-t", "host-a:%7.%8"],
        "",
        &[
            ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
            ("HUMANIZE_TMUX_BIN", fake_tmux.to_str().unwrap()),
            ("TMUX_PANE", "%1"),
        ],
    );
    assert!(allowed.status.success());
    assert!(
        fs::read_to_string(&calls)
            .unwrap()
            .contains("capture-pane -t host-a:%7.%8")
    );

    let blocked = run_plugin_with_stdin_env(
        &[
            "--guarded-tmux",
            "--",
            "send-keys",
            "-t",
            "host-a:%7.%8",
            "-l",
            "secret prompt text",
        ],
        "",
        &[
            ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
            ("HUMANIZE_TMUX_BIN", fake_tmux.to_str().unwrap()),
            ("TMUX_PANE", "%1"),
        ],
    );
    assert!(!blocked.status.success());
    assert!(!String::from_utf8_lossy(&blocked.stderr).contains("secret prompt text"));
    let records = read_hook_records(&root, "run-wrapper");
    let blocked_record = records
        .iter()
        .find(|record| record["fact"]["hook"] == "humanize.tmux_guard_blocked")
        .expect("blocked wrapper should record a hook fact");
    assert_eq!(blocked_record["fact"]["payload"]["decision"], "blocked");
    assert_eq!(
        blocked_record["fact"]["payload"]["target_run_id"],
        "run-wrapper"
    );
    assert_eq!(
        blocked_record["fact"]["payload"]["target_activation_id"],
        "root"
    );
    assert_eq!(blocked_record["fact"]["payload"]["operation"], "send-keys");
    assert_eq!(
        blocked_record["fact"]["payload"]["option_flags"],
        json!(["-l", "-t"])
    );
    assert!(
        blocked_record["fact"]["payload"]["payload_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(
        blocked_record["fact"]["payload"]["payload_length"],
        "secret prompt text".len()
    );
    assert!(!blocked_record.to_string().contains("secret prompt text"));
}

#[test]
fn agent_ready_hook_without_participant_binding_is_silent_and_writes_nothing() {
    let root = temp_root("cli-agent-ready");
    let owned = create_owned_pane_manifest(&root, "run-ready", "root");
    let readiness_nonce = owned.readiness_nonce.clone();

    let output = run_plugin_with_stdin_env(
        &["--agent-ready-hook", "--source", "codex_session_start"],
        "",
        &[
            ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
            ("TMUX_PANE", "%8"),
            ("HUMANIZE_READY_RUN_ID", "run-ready"),
            ("HUMANIZE_READY_ACTIVATION_ID", "root"),
            ("HUMANIZE_READY_ALLOCATION_GENERATION", "0"),
            ("HUMANIZE_READY_NONCE", readiness_nonce.as_str()),
        ],
    );

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.clone()))
        .run_root("run-ready")
        .unwrap();
    assert!(!run_root.join("records/hook.jsonl").exists());
}

#[test]
fn generated_session_start_hooks_are_silent_with_or_without_tmux_pane() {
    let command = env!("CARGO_BIN_EXE_humanize-plugin-mcp");
    for target in ["codex-hooks-json", "claude-hooks-json"] {
        let rendered = run_plugin(&["--print-client-config", target, "--command", command]);
        assert!(rendered.status.success());
        let config: Value = serde_json::from_slice(&rendered.stdout).unwrap();
        let hook = config["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let root = temp_root(&format!("generated-session-start-{target}"));
        let owned = create_owned_pane_manifest(&root, "run-generated-ready", "root");
        let readiness_nonce = owned.readiness_nonce.clone();

        let absent = Command::new("sh")
            .arg("-c")
            .arg(hook)
            .env("HUMANIZE_RUNS_DIR", &root)
            .env_remove("TMUX_PANE")
            .output()
            .unwrap();
        assert_eq!(absent.status.code(), Some(0), "{target}");
        assert_eq!(String::from_utf8_lossy(&absent.stdout), "");
        assert_eq!(String::from_utf8_lossy(&absent.stderr), "");

        let unrelated = Command::new("sh")
            .arg("-c")
            .arg(hook)
            .env("HUMANIZE_RUNS_DIR", &root)
            .env("TMUX_PANE", "%99")
            .env("HUMANIZE_READY_RUN_ID", "run-generated-ready")
            .env("HUMANIZE_READY_ACTIVATION_ID", "root")
            .env("HUMANIZE_READY_ALLOCATION_GENERATION", "0")
            .env("HUMANIZE_READY_NONCE", &readiness_nonce)
            .output()
            .unwrap();
        assert_eq!(unrelated.status.code(), Some(0), "{target}");
        assert_eq!(String::from_utf8_lossy(&unrelated.stdout), "");
        assert_eq!(String::from_utf8_lossy(&unrelated.stderr), "");
        let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.clone()))
            .run_root("run-generated-ready")
            .unwrap();
        assert!(!run_root.join("records/hook.jsonl").exists());

        let ready = Command::new("sh")
            .arg("-c")
            .arg(hook)
            .env("HUMANIZE_RUNS_DIR", &root)
            .env("TMUX_PANE", "%8")
            .env("HUMANIZE_READY_RUN_ID", "run-generated-ready")
            .env("HUMANIZE_READY_ACTIVATION_ID", "root")
            .env("HUMANIZE_READY_ALLOCATION_GENERATION", "0")
            .env("HUMANIZE_READY_NONCE", &readiness_nonce)
            .output()
            .unwrap();
        assert_eq!(ready.status.code(), Some(0), "{target}");
        assert_eq!(String::from_utf8_lossy(&ready.stdout), "");
        assert_eq!(String::from_utf8_lossy(&ready.stderr), "");

        assert!(!run_root.join("records/hook.jsonl").exists());
    }
}

#[test]
fn agent_ready_hook_ignores_unowned_pane_without_creating_runs() {
    let root = temp_root("cli-agent-ready-unknown");

    let output = run_plugin_with_stdin_env(
        &["--agent-ready-hook", "--source", "codex_session_start"],
        "",
        &[
            ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
            ("TMUX_PANE", "%99"),
        ],
    );

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
}

#[test]
fn agent_ready_hook_rejects_mismatched_allocation_identity_for_owned_pane() {
    let root = temp_root("cli-agent-ready-mismatched-allocation");
    let _owned = create_owned_pane_manifest(&root, "run-ready", "root");

    let output = run_plugin_with_stdin_env(
        &["--agent-ready-hook", "--source", "codex_session_start"],
        "",
        &[
            ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
            ("TMUX_PANE", "%8"),
            ("HUMANIZE_READY_RUN_ID", "run-ready"),
            ("HUMANIZE_READY_ACTIVATION_ID", "root"),
            ("HUMANIZE_READY_ALLOCATION_GENERATION", "99"),
            ("HUMANIZE_READY_NONCE", "stale-allocation"),
        ],
    );

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    let run_root = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.clone()))
        .run_root("run-ready")
        .unwrap();
    assert!(!run_root.join("records/hook.jsonl").exists());
}

#[test]
fn session_start_without_binding_cannot_satisfy_any_allocation() {
    let root = temp_root("cli-agent-ready-replaced-allocation");
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.clone()));
    let mut manifest = store.start_run_manifest("run-ready").unwrap();
    store
        .start_activation_capture(
            &mut manifest,
            RunAssetActivationUpdate {
                activation_id: "root".to_string(),
                node_id: "root".to_string(),
                adapter: "tmux".to_string(),
                tmux: RunAssetTmuxTarget {
                    session_id: "host-a".into(),
                    window_id: "%7".into(),
                    window_name: "flow-a".into(),
                    pane_id: "%8".into(),
                    allocation_generation: 0,
                },
                termination_reason: None,
            },
        )
        .unwrap();
    let old_nonce = manifest.activations["root"].readiness_nonce.clone();
    store
        .start_activation_capture(
            &mut manifest,
            RunAssetActivationUpdate {
                activation_id: "root".to_string(),
                node_id: "root".to_string(),
                adapter: "tmux".to_string(),
                tmux: RunAssetTmuxTarget {
                    session_id: "host-a".into(),
                    window_id: "%7".into(),
                    window_name: "flow-a".into(),
                    pane_id: "%8".into(),
                    allocation_generation: 1,
                },
                termination_reason: None,
            },
        )
        .unwrap();
    let current_nonce = manifest.activations["root"].readiness_nonce.clone();
    assert_ne!(old_nonce, current_nonce);

    let stale = run_plugin_with_stdin_env(
        &["--agent-ready-hook", "--source", "codex_session_start"],
        "",
        &[
            ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
            ("TMUX_PANE", "%8"),
            ("HUMANIZE_READY_RUN_ID", "run-ready"),
            ("HUMANIZE_READY_ACTIVATION_ID", "root"),
            ("HUMANIZE_READY_ALLOCATION_GENERATION", "0"),
            ("HUMANIZE_READY_NONCE", old_nonce.as_str()),
        ],
    );
    assert!(stale.status.success());
    assert_eq!(String::from_utf8_lossy(&stale.stdout), "");
    assert_eq!(String::from_utf8_lossy(&stale.stderr), "");
    let run_root = store.run_root("run-ready").unwrap();
    assert!(!run_root.join("records/hook.jsonl").exists());

    let current = run_plugin_with_stdin_env(
        &["--agent-ready-hook", "--source", "codex_session_start"],
        "",
        &[
            ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
            ("TMUX_PANE", "%8"),
            ("HUMANIZE_READY_RUN_ID", "run-ready"),
            ("HUMANIZE_READY_ACTIVATION_ID", "root"),
            ("HUMANIZE_READY_ALLOCATION_GENERATION", "1"),
            ("HUMANIZE_READY_NONCE", current_nonce.as_str()),
        ],
    );
    assert!(current.status.success());
    assert!(!run_root.join("records/hook.jsonl").exists());
}

#[test]
fn agent_ready_hook_without_binding_does_not_read_corrupt_manifest() {
    let root = temp_root("cli-agent-ready-corrupt");
    let owned = create_owned_pane_manifest(&root, "run-ready-corrupt", "root");
    let readiness_nonce = owned.readiness_nonce.clone();
    let run_root = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
    fs::write(run_root.join("manifest.json"), b"{not-json\n").unwrap();

    let output = run_plugin_with_stdin_env(
        &["--agent-ready-hook", "--source", "codex_session_start"],
        "",
        &[
            ("HUMANIZE_RUNS_DIR", root.to_str().unwrap()),
            ("TMUX_PANE", "%8"),
            ("HUMANIZE_READY_RUN_ID", "run-ready-corrupt"),
            ("HUMANIZE_READY_ACTIVATION_ID", "root"),
            ("HUMANIZE_READY_ALLOCATION_GENERATION", "0"),
            ("HUMANIZE_READY_NONCE", readiness_nonce.as_str()),
        ],
    );

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
