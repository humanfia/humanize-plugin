#[path = "support/driver_flows.rs"]
#[allow(dead_code)]
mod driver_flows;
#[path = "support/driver_tmux.rs"]
mod driver_tmux_support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::driver::socket_path_for_run_root;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use driver_flows::{approved_review_id, reviewed_lock_package, routed_locked_flow};
use driver_tmux_support::{ControlledTmuxFixture, capture_identity_is_alive};

#[test]
fn restart_adopts_first_window_and_pane_created_before_receipt() {
    let fixture = DriverFixture::new("first-pane-intent");
    let fake_tmux = fixture.fake_tmux();
    let crash_marker = fixture.root.join("crash-first-pane");
    fs::write(&crash_marker, "crash").unwrap();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let crash_marker_value = crash_marker.to_string_lossy().to_string();
    let mut driver = fixture.spawn(
        "run-first-pane-intent",
        &[
            ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
            (
                "HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_IF_EXISTS",
                crash_marker_value.as_str(),
            ),
            (
                "HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_KIND",
                "pane_created",
            ),
        ],
    );
    fixture.request_expect_disconnect(
        &mut driver,
        json!({
            "id": "bind",
            "token": fixture.token,
            "op": "bind_run",
            "run_id": "run-first-pane-intent",
            "flow_lock": routed_locked_flow(),
            "run_mode": "continuous",
            "activation_limit": 4,
            "tmux": tmux_request()
        }),
    );
    drop(driver);

    let mut restarted = fixture.spawn(
        "run-first-pane-intent",
        &[("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str())],
    );
    let resumed = fixture.request(json!({
        "id": "resume",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-first-pane-intent"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(resumed["tmux_allocations"][0]["pane_id"], "%8");
    let log = fs::read_to_string(fixture.root.join("tmux.log")).unwrap();
    assert_eq!(
        log.matches("new-session").count() + log.matches("new-window").count(),
        1,
        "{log}"
    );
    restarted.shutdown();
}

#[test]
fn restart_adopts_split_pane_created_before_receipt() {
    let fixture = DriverFixture::new("split-pane-intent");
    let fake_tmux = fixture.fake_tmux();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let mut driver = fixture.spawn(
        "run-split-pane-intent",
        &[("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str())],
    );
    let bound = fixture.request(json!({
        "id": "bind",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": "run-split-pane-intent",
        "flow_lock": routed_locked_flow(),
        "run_mode": "continuous",
        "activation_limit": 4,
        "tmux": tmux_request()
    }));
    assert_eq!(bound["ok"], true, "{bound}");

    let crash_marker = fixture.root.join("crash-split-pane");
    fs::write(&crash_marker, "crash").unwrap();
    driver.crash();
    let crash_marker_value = crash_marker.to_string_lossy().to_string();
    let mut crashing = fixture.spawn(
        "run-split-pane-intent",
        &[
            ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
            (
                "HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_IF_EXISTS",
                crash_marker_value.as_str(),
            ),
            (
                "HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_KIND",
                "pane_created",
            ),
        ],
    );
    let resumed = fixture.request(json!({
        "id": "resume-before-route",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-split-pane-intent"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    fixture.request_expect_disconnect(
        &mut crashing,
        json!({
            "id": "deliver",
            "token": fixture.token,
            "op": "deliver_artifact",
            "run_id": "run-split-pane-intent",
            "activation_id": "root",
            "artifact_id": "brief",
            "payload": "ready"
        }),
    );
    drop(crashing);

    let mut restarted = fixture.spawn(
        "run-split-pane-intent",
        &[("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str())],
    );
    let resumed = fixture.request(json!({
        "id": "resume-after-crash",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-split-pane-intent"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert!(
        resumed["tmux_allocations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|pane| { pane["activation_id"] == "follow" && pane["pane_id"] == "%9" })
    );
    let log = fs::read_to_string(fixture.root.join("tmux.log")).unwrap();
    assert_eq!(log.matches("split-window").count(), 1, "{log}");
    restarted.shutdown();
}

#[test]
fn restart_replays_capture_intent_without_starting_a_second_physical_capture() {
    let fixture = DriverFixture::new("capture-intent");
    let fake_tmux = fixture.fake_tmux();
    let crash_marker = fixture.root.join("crash-capture");
    fs::write(&crash_marker, "crash").unwrap();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let crash_marker_value = crash_marker.to_string_lossy().to_string();
    let mut driver = fixture.spawn(
        "run-capture-intent",
        &[
            ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
            (
                "HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_IF_EXISTS",
                crash_marker_value.as_str(),
            ),
            (
                "HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_KIND",
                "capture_started",
            ),
        ],
    );
    fixture.request_expect_disconnect(
        &mut driver,
        json!({
            "id": "bind",
            "token": fixture.token,
            "op": "bind_run",
            "run_id": "run-capture-intent",
            "flow_lock": reviewed_lock_package(),
            "run_mode": "continuous",
            "activation_limit": 4,
            "tmux": tmux_request()
        }),
    );
    drop(driver);

    let mut restarted = fixture.spawn(
        "run-capture-intent",
        &[
            ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
            ("HUMANIZE_DRIVER_AGENT_READY_TIMEOUT_MS", "50"),
        ],
    );
    let resumed = fixture.request(json!({
        "id": "resume",
        "token": fixture.token,
        "op": "resume",
        "run_id": "run-capture-intent"
    }));
    assert_eq!(resumed["ok"], true, "{resumed}");
    let starts = fs::read_to_string(fixture.root.join("capture-starts")).unwrap();
    assert_eq!(starts.lines().count(), 1, "{starts}");
    restarted.shutdown();
}

#[test]
fn dropping_fixture_reaps_capture_helpers_left_by_a_driver_crash() {
    let fixture = DriverFixture::new("capture-helper-cleanup");
    let fake_tmux = fixture.fake_tmux();
    let crash_marker = fixture.root.join("crash-capture");
    fs::write(&crash_marker, "crash").unwrap();
    let fake_tmux_value = fake_tmux.to_string_lossy().to_string();
    let crash_marker_value = crash_marker.to_string_lossy().to_string();
    let mut driver = fixture.spawn(
        "run-capture-helper-cleanup",
        &[
            ("HUMANIZE_TMUX_BIN", fake_tmux_value.as_str()),
            (
                "HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_IF_EXISTS",
                crash_marker_value.as_str(),
            ),
            (
                "HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_KIND",
                "capture_started",
            ),
        ],
    );
    fixture.request_expect_disconnect(
        &mut driver,
        json!({
            "id": "bind",
            "token": fixture.token,
            "op": "bind_run",
            "run_id": "run-capture-helper-cleanup",
            "flow_lock": reviewed_lock_package(),
            "run_mode": "continuous",
            "activation_limit": 4,
            "tmux": tmux_request()
        }),
    );
    let identity = fixture.tmux_control.capture_identity("8").unwrap();
    assert!(capture_identity_is_alive(&identity));
    let root = fixture.root.clone();
    drop(driver);
    drop(fixture);

    assert!(!capture_identity_is_alive(&identity));
    let done = fs::read_to_string(root.join("pipe-8.done")).unwrap();
    assert_eq!(
        done.split_whitespace().next(),
        Some(identity.nonce.as_str())
    );
}

fn tmux_request() -> Value {
    json!({
        "enabled": true,
        "session": "host-a",
        "window": "flow-a",
        "agent_command": "humanize-test-agent"
    })
}

struct DriverFixture {
    root: PathBuf,
    token: &'static str,
    tmux_control: ControlledTmuxFixture,
}

impl DriverFixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .join("humanize-effect-recovery")
            .join(format!("{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(root.join("runtime")).unwrap();
        fs::create_dir_all(root.join("runs")).unwrap();
        let tmux_control = ControlledTmuxFixture::new(&root);
        Self {
            root,
            token: "test-token",
            tmux_control,
        }
    }

    fn spawn(&self, run_id: &str, envs: &[(&str, &str)]) -> DriverProcess {
        let mut command = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-driver"));
        command
            .arg("--run-id")
            .arg(run_id)
            .arg("--runs-root")
            .arg(self.root.join("runs"))
            .arg("--runtime-root")
            .arg(self.root.join("runtime"))
            .arg("--review-root")
            .arg(self.root.join("reviews"))
            .arg("--auth-token")
            .arg(self.token)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .env("HUMANIZE_STATE_ROOT", &self.root);
        for (key, value) in envs {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        wait_for_socket(&mut child, &self.socket_path(run_id));
        DriverProcess { child }
    }

    fn request(&self, request: Value) -> Value {
        let request = self.with_review_id(request);
        let run_id = request["run_id"].as_str().unwrap();
        let mut stream = UnixStream::connect(self.socket_path(run_id)).unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    }

    fn request_expect_disconnect(&self, driver: &mut DriverProcess, request: Value) {
        let request = self.with_review_id(request);
        let run_id = request["run_id"].as_str().unwrap();
        let mut stream = UnixStream::connect(self.socket_path(run_id)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut response = String::new();
        let _ = BufReader::new(stream).read_line(&mut response);
        assert!(
            response.is_empty(),
            "driver unexpectedly responded: {response}"
        );
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if driver.child.try_wait().unwrap().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("driver did not exit at the injected tmux crash boundary");
    }

    fn with_review_id(&self, mut request: Value) -> Value {
        if request.get("flow_lock").is_some()
            && request.get("review_id").is_none()
            && request.get("reviewId").is_none()
        {
            let review_id = approved_review_id(&self.root.join("reviews"), &request["flow_lock"]);
            request["review_id"] = Value::String(review_id);
        }
        request
    }

    fn socket_path(&self, run_id: &str) -> PathBuf {
        socket_path_for_run_root(&self.root.join("runtime"), &self.run_root(run_id))
    }

    fn run_root(&self, run_id: &str) -> PathBuf {
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(self.root.join("runs")))
            .run_root(run_id)
            .unwrap()
    }

    fn fake_tmux(&self) -> PathBuf {
        let path = self.root.join("fake-tmux");
        let script = format!(
            r#"#!/bin/sh
root='{}'
printf '%s\n' "$*" >> "$root/tmux.log"
last=''
target=''
previous=''
for arg in "$@"; do
  if test "$previous" = '-t'; then target="$arg"; fi
  previous="$arg"
  last="$arg"
done
next_pane() {{
  if test -f "$root/pane.counter"; then pane="$(( $(cat "$root/pane.counter") + 1 ))"; else pane='8'; fi
  printf '%s\n' "$pane" > "$root/pane.counter"
  printf '%%%s' "$pane"
}}
record_pane() {{
  printf '%s\t%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' "$1" "$2" >> "$root/panes"
}}
case "$1" in
  has-session)
    test -f "$root/session.exists"
    ;;
  new-session)
    : > "$root/session.exists"
    pane="$(next_pane)"
    record_pane "$pane" "$last"
    printf '%s\t%s\n' '%7' "$pane"
    ;;
  new-window)
    pane="$(next_pane)"
    record_pane "$pane" "$last"
    printf '%s\t%s\n' '%7' "$pane"
    ;;
  split-window)
    pane="$(next_pane)"
    record_pane "$pane" "$last"
    printf '%s\n' "$pane"
    ;;
  list-panes)
    test -f "$root/panes" && cat "$root/panes"
    ;;
  display-message)
    pane="${{target##*.}}"
    printf '%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' "$pane"
    ;;
  pipe-pane)
    pane="${{target##*.}}"
    before="$(cat "$root/pipe-${{pane#%}}.owner" 2>/dev/null || true)"
    "$root/fake-pipe-capture" start "$root" "${{pane#%}}" "$last"
    after="$(cat "$root/pipe-${{pane#%}}.owner")"
    if test "$before" != "$after"; then
      printf '%s\n' "$target" >> "$root/capture-starts"
    fi
    ;;
  capture-pane)
    printf 'final capture for %s\n' "$target"
    ;;
  kill-pane)
    pane="${{target##*.}}"
    "$root/fake-pipe-capture" stop "$root" "${{pane#%}}"
    ;;
esac
exit 0
"#,
            self.root.display()
        );
        fs::write(&path, script).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

struct DriverProcess {
    child: Child,
}

impl DriverProcess {
    fn crash(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    fn shutdown(&mut self) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, "quit");
            let _ = stdin.flush();
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(3) {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        panic!("driver did not shut down cleanly");
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
            panic!("driver exited before socket was ready: {status}; stderr={stderr}");
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("driver socket was not ready at {}", path.display());
}
