use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::driver::socket_path_for_run_root;
use humanize_plugin::flow::FlowLock;
use humanize_plugin::review::{ReviewDecision, ReviewStore};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use humanize_plugin::runtime::{self, ControlCommand, DriverTickInput, NodeSpec, StopContract};
use serde_json::{Value, json};

use crate::driver_tmux_support::ControlledTmuxFixture;

pub(super) struct DriverFixture {
    pub(super) root: PathBuf,
    pub(super) runtime_root: PathBuf,
    pub(super) token: &'static str,
    tmux_control: ControlledTmuxFixture,
}

impl DriverFixture {
    pub(super) fn new(name: &str) -> Self {
        let runtime_root = test_state_root().join("runtime");
        let root = std::env::temp_dir()
            .join("humanize-plugin-driver-tests")
            .join(format!("{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(root.join("runs")).unwrap();
        let tmux_control = ControlledTmuxFixture::new(&root);
        Self {
            root,
            runtime_root,
            token: "test-token",
            tmux_control,
        }
    }

    pub(super) fn spawn(&self, run_id: &str) -> DriverProcess {
        self.spawn_with_env(run_id, &[])
    }

    pub(super) fn spawn_with_env(&self, run_id: &str, envs: &[(&str, &Path)]) -> DriverProcess {
        let mut command = self.driver_command(run_id);
        for (key, value) in envs {
            command.env(key, value);
        }
        self.spawn_command(run_id, command)
    }

    pub(super) fn spawn_with_env_values(
        &self,
        run_id: &str,
        envs: &[(&str, &str)],
    ) -> DriverProcess {
        let mut command = self.driver_command(run_id);
        for (key, value) in envs {
            command.env(key, value);
        }
        self.spawn_command(run_id, command)
    }

    fn spawn_command(&self, run_id: &str, mut command: Command) -> DriverProcess {
        let mut child = command.spawn().unwrap();
        wait_for_socket(&mut child, &self.socket_path(run_id));
        let stdout = BufReader::new(child.stdout.take().unwrap());
        DriverProcess { child, stdout }
    }

    pub(super) fn spawn_until_exit(&self, run_id: &str, timeout: Duration) -> std::process::Output {
        let mut child = self.driver_command(run_id).spawn().unwrap();
        let started = Instant::now();
        while started.elapsed() < timeout {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        panic!(
            "driver did not exit within {:?}; stdout={}; stderr={}",
            timeout,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn driver_command(&self, run_id: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-driver"));
        command
            .arg("--run-id")
            .arg(run_id)
            .arg("--runs-root")
            .arg(self.root.join("runs"))
            .arg("--runtime-root")
            .arg(&self.runtime_root)
            .arg("--review-root")
            .arg(self.root.join("reviews"))
            .arg("--auth-token")
            .arg(self.token)
            .env("HUMANIZE_STATE_ROOT", test_state_root())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    pub(super) fn socket_path(&self, run_id: &str) -> PathBuf {
        socket_path_for_run_root(&self.runtime_root, &self.run_root(run_id))
            .expect("driver socket path should resolve")
    }

    pub(super) fn run_events_path(&self, run_id: &str) -> PathBuf {
        self.private_driver_dir(run_id).join("events.jsonl")
    }

    pub(super) fn driver_events_path(&self, run_id: &str) -> PathBuf {
        self.private_driver_dir(run_id).join("driver-events.jsonl")
    }

    pub(super) fn run_root(&self, run_id: &str) -> PathBuf {
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(self.root.join("runs")))
            .run_root(run_id)
            .unwrap()
    }

    pub(super) fn private_run_root(&self, run_id: &str) -> PathBuf {
        let run_root = self.run_root(run_id);
        let identity = std::path::absolute(&run_root)
            .unwrap_or(run_root)
            .to_string_lossy()
            .into_owned();
        self.runtime_root
            .join(format!("r{:016x}", stable_hash(&identity)))
    }

    pub(super) fn private_driver_dir(&self, run_id: &str) -> PathBuf {
        self.private_run_root(run_id).join("driver")
    }

    pub(super) fn single_revision(&self, run_id: &str) -> Value {
        let revisions_dir = self.private_driver_dir(run_id).join("revisions");
        let mut entries = fs::read_dir(&revisions_dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        entries.sort_by_key(|entry| entry.path());
        assert_eq!(entries.len(), 1);
        serde_json::from_slice(&fs::read(entries[0].path()).unwrap()).unwrap()
    }

    pub(super) fn tmux_log(&self) -> PathBuf {
        self.root.join("tmux.log")
    }

    pub(super) fn fake_tmux_with_agent_ready(&self) -> PathBuf {
        self.fake_tmux_for_actuation(true)
    }

    pub(super) fn fake_tmux_without_agent_ready(&self) -> PathBuf {
        self.fake_tmux_for_actuation(false)
    }

    fn fake_tmux_for_actuation(&self, agent_ready: bool) -> PathBuf {
        let path = self.root.join(if agent_ready {
            "fake-tmux-agent-ready"
        } else {
            "fake-tmux-no-agent-ready"
        });
        let script = format!(
            r#"#!/bin/sh
root='{}'
agent_ready='{}'
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
  command="$1"
  eval "set -- $command"
  if test "$1" = 'env'; then shift; fi
  while test "$#" -gt 0; do
    case "$1" in
      HUMANIZE_READY_RUN_ID=*|HUMANIZE_READY_ACTIVATION_ID=*|HUMANIZE_READY_ALLOCATION_GENERATION=*|HUMANIZE_READY_NONCE=*|HUMANIZE_PARTICIPANT_RUN_ID=*|HUMANIZE_PARTICIPANT_ACTIVATION_ID=*|HUMANIZE_PARTICIPANT_HANDLE=*|HUMANIZE_PARTICIPANT_CREDENTIAL=*|HUMANIZE_PARTICIPANT_BINDING_FILE=*|HUMANIZE_RUNS_DIR=*)
        export "$1"
        shift
        ;;
      *) break ;;
    esac
  done
}}
handle_input() {{
  input="$1"
  case "$input" in
    *humanize-test-agent*)
      if test -n "$HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT"; then
        : > "$HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT"
      fi
      if test "$agent_ready" = 'yes'; then
        load_ready_environment "$input"
        pane="${{target##*.}}"
        pending="$root/hook-helper-${{pane#%}}-$$.pending"
        done="${{pending%.pending}}.done"
        : > "$pending"
        (
          printf '%s\n' '{{"hook_event_name":"SessionStart","session_id":"fake-native-session"}}' |
            TMUX_PANE="$pane" '{}' --agent-ready-hook --source codex_session_start
          mv "$pending" "$done"
        ) </dev/null >> "$root/hook.out" 2>> "$root/hook.err" &
      fi
      ;;
  esac
}}
case "$1" in
  list-panes)
    test -f "$root/panes" && cat "$root/panes"
    ;;
  has-session)
    exit 1
    ;;
  new-session)
    printf '%s\t%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' '%8' "$last" > "$root/panes"
    printf '%s\t%s\n' '%7' '%8'
    ;;
  split-window)
    printf '%s\t%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' '%9' "$last" >> "$root/panes"
    printf '%s\n' '%9'
    ;;
  display-message)
    pane="${{target##*.}}"
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
    if test -f "$root/panes"; then
      awk -F '\t' -v pane="$pane" '$4 != pane' "$root/panes" > "$root/panes.next"
      mv "$root/panes.next" "$root/panes"
    fi
    ;;
esac
exit 0
"#,
            self.root.display(),
            if agent_ready { "yes" } else { "no" },
            env!("CARGO_BIN_EXE_humanize-plugin-mcp")
        );
        fs::write(&path, script).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    pub(super) fn request(&self, mut request: Value) -> Value {
        if request.get("flow_lock").is_some()
            && request.get("review_id").is_none()
            && request.get("reviewId").is_none()
        {
            let lock = serde_json::from_value::<FlowLock>(request["flow_lock"].clone()).unwrap();
            let store = ReviewStore::new(self.root.join("reviews"));
            let review = store
                .prepare(
                    &lock,
                    &json!({"title":"Driver fixture review"}),
                    "<title>Driver fixture review</title>\n",
                )
                .unwrap();
            let review = store
                .decide(review.review_id(), ReviewDecision::Approved, None)
                .unwrap();
            request["review_id"] = Value::String(review.review_id().to_string());
        }
        self.raw_request(&(request.to_string() + "\n"))
    }

    pub(super) fn raw_request(&self, request: &str) -> Value {
        let value = serde_json::from_str::<Value>(request.trim()).unwrap_or_else(|_| {
            json!({
                "run_id": "run-errors"
            })
        });
        let run_id = value
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or("run-errors");
        let mut stream = UnixStream::connect(self.socket_path(run_id)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        assert!(self.tmux_control.wait_for_hooks());
        serde_json::from_str(&response).unwrap()
    }
}

fn test_state_root() -> &'static PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!(
            "humanize-plugin-driver-state-{}",
            std::process::id()
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(root.join("runtime")).unwrap();
        set_mode(&root, 0o700);
        set_mode(&root.join("runtime"), 0o700);
        unsafe {
            std::env::set_var("HUMANIZE_STATE_ROOT", &root);
        }
        root
    })
}

fn set_mode(path: &Path, mode: u32) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions).unwrap();
}

pub(super) struct DriverProcess {
    child: Child,
    stdout: BufReader<std::process::ChildStdout>,
}

impl DriverProcess {
    pub(super) fn console(&mut self, command: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{command}").unwrap();
        stdin.flush().unwrap();
    }

    pub(super) fn read_console_until(&mut self, needle: &str, timeout: Duration) -> Option<String> {
        let started = Instant::now();
        let mut line = String::new();
        while started.elapsed() < timeout {
            line.clear();
            if self.stdout.read_line(&mut line).ok()? == 0 {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            if line.contains(needle) {
                return Some(line.clone());
            }
        }
        None
    }

    pub(super) fn shutdown(&mut self) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, "quit");
            let _ = stdin.flush();
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub(super) fn crash(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }

    pub(super) fn wait_for_exit(&mut self, timeout: Duration) {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("driver did not exit after shutdown");
    }
}

impl Drop for DriverProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn wait_for_socket(child: &mut Child, path: &Path) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(stream) = child.stderr.as_mut() {
                let _ = std::io::Read::read_to_string(stream, &mut stderr);
            }
            panic!(
                "driver exited before socket was ready at {}: {status}; stderr={stderr}",
                path.display()
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("driver socket was not ready at {}", path.display());
}

pub(super) fn expected_initial_activation_ids() -> Vec<String> {
    let mut state = runtime::DriverState::default();
    let report = state.tick(
        DriverTickInput::default().with_control(ControlCommand::StartRun {
            run_id: "run-runtime-initial".into(),
            nodes: vec![
                NodeSpec::new("root")
                    .with_stop_contract(StopContract::new(["brief"], Vec::<&str>::new())),
            ],
        }),
    );
    assert_eq!(
        report
            .render
            .run_statuses
            .get("run-runtime-initial")
            .copied(),
        Some(runtime::RunStatus::Running)
    );
    state
        .runtime()
        .state()
        .activations
        .values()
        .map(|activation| activation.activation_id.clone())
        .collect()
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
