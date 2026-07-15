use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use humanize_plugin::tmux_guard::{
    ShellTmuxGuardDecision, TmuxSendBlock, TmuxSendGuardDecision, classify_shell_tmux_sends,
    classify_tmux_send, classify_tmux_send_from_runs_dir, classify_tmux_send_with_context,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
static STATE_ENV_LOCK: Mutex<()> = Mutex::new(());

fn argv(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn owned_panes(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn temp_root(name: &str) -> PathBuf {
    let index = NEXT_TEMP.fetch_add(1, Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("{name}-{index}"));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    root
}

fn create_owned_pane_manifest(root: &Path, cleanup_status: &str) {
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.to_path_buf()));
    let manifest = store.start_run_manifest("run-owned").unwrap();
    let driver_dir = create_private_run_identity(root, &manifest.root, "run-owned");
    let mut events = vec![serde_json::json!({
        "seq": 1,
        "at_ms": 1,
        "kind": "tmux_pane_allocated",
        "payload": {
            "activation_id": "root",
            "pane": {
                "session_id": "host-a",
                "window_id": "%7",
                "window_name": "flow-a",
                "pane_id": "%8",
                "allocation_generation": 0
            }
        }
    })];
    if cleanup_status == "complete" {
        events.push(serde_json::json!({
            "seq": 2,
            "at_ms": 2,
            "kind": "tmux_pane_cleanup_receipt",
            "payload": {"activation_id": "root"}
        }));
    }
    write_private_jsonl(&driver_dir.join("driver-events.jsonl"), &events);
}

#[test]
fn blocks_tmux_send_keys_to_owned_target() {
    let decision = classify_tmux_send(
        &argv(&[
            "tmux",
            "send-keys",
            "-t",
            "host-a:%7.%8",
            "-l",
            "inspect the repo",
        ]),
        &owned_panes(&["host-a:%7.%8"]),
    );

    assert_eq!(
        decision,
        TmuxSendGuardDecision::Blocked(TmuxSendBlock {
            target: "host-a:%7.%8".to_string(),
            reason: "Direct tmux send to Humanize-owned pane host-a:%7.%8 is blocked. Use Humanize MCP or the Humanize input tool so machine input is recorded."
                .to_string(),
        })
    );
}

#[test]
fn discovers_live_owned_panes_from_private_driver_events() {
    let root = temp_root("tmux-guard-discovery");
    let _state = StateRootGuard::new(&private_state_root(&root));
    create_owned_pane_manifest(&root, "pending");

    let decision = classify_tmux_send_from_runs_dir(
        &argv(&["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"]),
        &root,
        None,
    )
    .unwrap();

    assert!(
        matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "host-a:%7.%8")
    );
}

#[test]
fn private_cleanup_receipts_remove_owned_panes_from_discovery() {
    let root = temp_root("tmux-guard-released");
    let _state = StateRootGuard::new(&private_state_root(&root));
    create_owned_pane_manifest(&root, "complete");

    let decision = classify_tmux_send_from_runs_dir(
        &argv(&["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"]),
        &root,
        None,
    )
    .unwrap();

    assert_eq!(decision, TmuxSendGuardDecision::Allowed);
}

#[test]
fn driver_event_ownership_is_discovered_until_release() {
    let root = temp_root("tmux-guard-driver-pane");
    let _state = StateRootGuard::new(&private_state_root(&root));
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.clone()));
    let manifest = store.start_run_manifest("run-driver-owned").unwrap();
    let driver_dir = create_private_run_identity(&root, &manifest.root, "run-driver-owned");
    let events = driver_dir.join("driver-events.jsonl");
    let mut file = fs::File::create(&events).unwrap();
    fs::set_permissions(&events, fs::Permissions::from_mode(0o600)).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "seq": 1,
            "at_ms": 1,
            "kind": "driver_pane_owned",
            "payload": {
                "pane": {
                    "session_id": "host-a",
                    "window_id": "%7",
                    "window_name": "flow-a",
                    "pane_id": "%8"
                }
            }
        })
    )
    .unwrap();
    file.sync_all().unwrap();

    let owned = classify_tmux_send_from_runs_dir(
        &argv(&["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"]),
        &root,
        None,
    )
    .unwrap();
    assert!(matches!(owned, TmuxSendGuardDecision::Blocked(_)));

    writeln!(
        file,
        "{}",
        serde_json::json!({
            "seq": 2,
            "at_ms": 2,
            "kind": "driver_pane_released",
            "payload": {}
        })
    )
    .unwrap();
    file.sync_all().unwrap();
    let released = classify_tmux_send_from_runs_dir(
        &argv(&["tmux", "send-keys", "-t", "host-a:%7.%8", "Enter"]),
        &root,
        None,
    )
    .unwrap();
    assert_eq!(released, TmuxSendGuardDecision::Allowed);
}

fn create_private_run_identity(root: &Path, run_root: &Path, run_id: &str) -> PathBuf {
    let runtime_root =
        PathBuf::from(std::env::var_os("HUMANIZE_STATE_ROOT").unwrap()).join("runtime");
    fs::create_dir_all(&runtime_root).unwrap();
    fs::set_permissions(&runtime_root, fs::Permissions::from_mode(0o700)).unwrap();
    let identity = std::path::absolute(run_root)
        .unwrap_or_else(|_| run_root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let private_run_root = runtime_root.join(format!("r{:016x}", stable_hash(&identity)));
    fs::create_dir_all(&private_run_root).unwrap();
    fs::set_permissions(&private_run_root, fs::Permissions::from_mode(0o700)).unwrap();
    let public_run_root = std::path::absolute(run_root).unwrap();
    let runs_root = std::path::absolute(root).unwrap();
    let identity_path = private_run_root.join("identity.json");
    fs::write(
        &identity_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "humanize.private_run_identity.v1",
            "run_id": run_id,
            "public_run_root": public_run_root,
            "runs_root": runs_root
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&identity_path, fs::Permissions::from_mode(0o600)).unwrap();
    let driver_dir = private_run_root.join("driver");
    fs::create_dir_all(&driver_dir).unwrap();
    fs::set_permissions(&driver_dir, fs::Permissions::from_mode(0o700)).unwrap();
    driver_dir
}

fn write_private_jsonl(path: &Path, events: &[serde_json::Value]) {
    let mut file = fs::File::create(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    for event in events {
        writeln!(file, "{event}").unwrap();
    }
    file.sync_all().unwrap();
}

struct StateRootGuard {
    prior: Option<OsString>,
    root: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

impl StateRootGuard {
    fn new(root: &Path) -> Self {
        let guard = STATE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let prior = std::env::var_os("HUMANIZE_STATE_ROOT");
        unsafe {
            std::env::set_var("HUMANIZE_STATE_ROOT", root);
        }
        Self {
            prior,
            root: root.to_path_buf(),
            _guard: guard,
        }
    }
}

impl Drop for StateRootGuard {
    fn drop(&mut self) {
        unsafe {
            match self.prior.take() {
                Some(value) => std::env::set_var("HUMANIZE_STATE_ROOT", value),
                None => std::env::remove_var("HUMANIZE_STATE_ROOT"),
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn private_state_root(run_root: &Path) -> PathBuf {
    std::env::temp_dir()
        .join("humanize-plugin-tmux-guard-state")
        .join(run_root.file_name().unwrap())
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[test]
fn blocks_tmux_send_alias_after_global_flags() {
    let decision = classify_tmux_send(
        &argv(&[
            "tmux",
            "-L",
            "humanize-test",
            "-S",
            "/tmp/humanize-tmux.sock",
            "send",
            "-t",
            "%12",
            "Enter",
        ]),
        &owned_panes(&["%12"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%12"));
}

#[test]
fn blocks_tmux_send_with_attached_global_and_target_options() {
    let decision = classify_tmux_send(
        &argv(&[
            "/usr/bin/tmux",
            "-Lhumanize-test",
            "send-keys",
            "-t=%12",
            "-l",
            "inspect the repo",
        ]),
        &owned_panes(&["%12"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%12"));

    let decision = classify_tmux_send(
        &argv(&["tmux", "send", "-t%13", "Enter"]),
        &owned_panes(&["%13"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%13"));
}

#[test]
fn blocks_tmux_send_after_value_taking_global_flags() {
    let decision = classify_tmux_send(
        &argv(&[
            "tmux",
            "-c",
            "/tmp",
            "-Tfeatures",
            "send-key",
            "-t",
            "%12",
            "Enter",
        ]),
        &owned_panes(&["%12"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%12"));
}

#[test]
fn blocks_tmux_send_after_combined_no_value_global_flags() {
    let decision = classify_tmux_send(
        &argv(&[
            "tmux",
            "-vv",
            "-CC",
            "send-key",
            "-t",
            "host-a:%7.%8",
            "Enter",
        ]),
        &owned_panes(&["host-a:%7.%8"]),
    );

    assert!(
        matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "host-a:%7.%8")
    );
}

#[test]
fn blocks_tmux_send_to_owned_pane_id_alias() {
    let decision = classify_tmux_send(
        &argv(&["tmux", "send-keys", "-t", "%8", "-l", "inspect the repo"]),
        &owned_panes(&["host-a:%7.%8"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%8"));

    let decision = classify_tmux_send(
        &argv(&["tmux", "send-keys", "-t", "host-a:%7.%9", "Enter"]),
        &owned_panes(&["%9"]),
    );

    assert!(
        matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "host-a:%7.%9")
    );
}

#[test]
fn blocks_tmux_send_to_owned_window_or_session_target() {
    let decision = classify_tmux_send(
        &argv(&["tmux", "send-keys", "-t", "host-a:%7", "Enter"]),
        &owned_panes(&["host-a:%7.%8"]),
    );

    assert!(
        matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "host-a:%7")
    );

    let decision = classify_tmux_send(
        &argv(&["tmux", "send-keys", "-t", "host-a", "Enter"]),
        &owned_panes(&["host-a:%7.%8"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "host-a"));
}

#[test]
fn blocks_tmux_send_without_target_when_current_pane_is_owned() {
    let decision = classify_tmux_send_with_context(
        &argv(&["tmux", "send-keys", "-l", "inspect the repo"]),
        &owned_panes(&["host-a:%7.%8"]),
        Some("%8"),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%8"));
}

#[test]
fn allows_tmux_send_without_owned_explicit_target() {
    assert_eq!(
        classify_tmux_send(
            &argv(&["tmux", "send-keys", "-l", "inspect the repo"]),
            &owned_panes(&["host-a:%7.%8"]),
        ),
        TmuxSendGuardDecision::Allowed
    );
    assert_eq!(
        classify_tmux_send(
            &argv(&["tmux", "send-keys", "-t", "other:%1.%2", "Enter"]),
            &owned_panes(&["host-a:%7.%8"]),
        ),
        TmuxSendGuardDecision::Allowed
    );
}

#[test]
fn allows_unknown_or_non_send_commands() {
    let owned = owned_panes(&["host-a:%7.%8"]);

    assert_eq!(
        classify_tmux_send(
            &argv(&["tmux", "capture-pane", "-t", "host-a:%7.%8"]),
            &owned,
        ),
        TmuxSendGuardDecision::Allowed
    );
    assert_eq!(
        classify_tmux_send(
            &argv(&["ssh", "host", "tmux", "send-keys", "-t", "host-a:%7.%8"]),
            &owned,
        ),
        TmuxSendGuardDecision::Allowed
    );
}

#[test]
fn shell_guard_inspects_every_compound_command_and_rejects_invalid_syntax() {
    let owned = owned_panes(&["host-a:%7.%8"]);
    for command in [
        "true && tmux send-keys -t host-a:%7.%8 Enter",
        "printf ready | tmux send-keys -t host-a:%7.%8 -l payload",
        "echo $(tmux send-keys -t host-a:%7.%8 Enter)",
        "sh -c 'tmux send-keys -t host-a:%7.%8 Enter'",
    ] {
        assert!(matches!(
            classify_shell_tmux_sends(command, &owned, Some("%1")).unwrap(),
            ShellTmuxGuardDecision::Blocked { block, .. }
                if block.target == "host-a:%7.%8"
        ));
    }
    assert!(classify_shell_tmux_sends("echo 'unterminated", &owned, Some("%1")).is_err());
}

#[test]
fn shell_guard_normalizes_tmux_option_clusters_wrappers_and_modifiers() {
    let owned = owned_panes(&["host-a:%7.%8"]);
    for command in [
        "tmux send-keys -lt host-a:%7.%8 payload",
        "exec tmux send-keys -t host-a:%7.%8 Enter",
        "exec -a humanize-tmux tmux send-keys -t host-a:%7.%8 Enter",
        "env -i -u HOME PATH=/usr/bin tmux send-keys -t host-a:%7.%8 Enter",
        "env --unset=HOME -- tmux send-keys -t host-a:%7.%8 Enter",
        "command -p tmux send-keys -t host-a:%7.%8 Enter",
        "builtin exec tmux send-keys -t host-a:%7.%8 Enter",
        "MODE=guard exec env -i tmux send-keys -t host-a:%7.%8 Enter",
        "! tmux send-keys -t host-a:%7.%8 Enter",
        "time tmux send-keys -t host-a:%7.%8 Enter",
    ] {
        assert!(
            matches!(
                classify_shell_tmux_sends(command, &owned, Some("%1")).unwrap(),
                ShellTmuxGuardDecision::Blocked { block, .. }
                    if block.target == "host-a:%7.%8"
            ),
            "command bypassed guard: {command}"
        );
    }

    assert!(matches!(
        classify_tmux_send(
            &argv(&[
                "tmux",
                "send-keys",
                "-lt",
                "host-a:%7.%8",
                "payload"
            ]),
            &owned,
        ),
        TmuxSendGuardDecision::Blocked(block) if block.target == "host-a:%7.%8"
    ));
}

#[test]
fn shell_guard_recursively_inspects_literal_eval_and_rejects_dynamic_eval() {
    let owned = owned_panes(&["host-a:%7.%8"]);
    for command in [
        "eval 'tmux send-keys -lt host-a:%7.%8 eval-secret'",
        "eval 'eval \"tmux send-keys -t host-a:%7.%8 Enter\"'",
        "command eval 'exec tmux send-keys -t host-a:%7.%8 Enter'",
        "builtin eval 'tmux send-keys -t host-a:%7.%8 Enter'",
    ] {
        assert!(
            matches!(
                classify_shell_tmux_sends(command, &owned, Some("%1")).unwrap(),
                ShellTmuxGuardDecision::Blocked { block, .. }
                    if block.target == "host-a:%7.%8"
            ),
            "literal eval bypassed guard: {command}"
        );
    }

    for command in [
        "eval \"$TMUX_COMMAND\"",
        "eval 'tmux send-keys -t host-a:%7.%8' \"$TMUX_PAYLOAD\"",
        "eval \"$(printf 'tmux send-keys -t host-a:%7.%8 Enter')\"",
    ] {
        assert!(
            classify_shell_tmux_sends(command, &owned, Some("%1")).is_err(),
            "dynamic eval was not rejected: {command}"
        );
    }
}

#[test]
fn shell_guard_rejects_incomplete_or_semantically_dynamic_wrapper_options() {
    let owned = owned_panes(&["host-a:%7.%8"]);
    for command in [
        "env -u",
        "exec -a",
        "env --split-string 'tmux send-keys -t host-a:%7.%8 Enter'",
    ] {
        assert!(
            classify_shell_tmux_sends(command, &owned, Some("%1")).is_err(),
            "unsupported wrapper syntax was not rejected: {command}"
        );
    }
}
