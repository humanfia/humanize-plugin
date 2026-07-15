// Each integration test crate compiles this support module independently.
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    pub pid: i32,
    pub start_time: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureIdentity {
    pub nonce: String,
    pub supervisor: ProcessIdentity,
    pub sink: ProcessIdentity,
    pub writer: ProcessIdentity,
}

pub struct ControlledTmuxFixture {
    root: PathBuf,
}

impl ControlledTmuxFixture {
    pub fn new(root: &Path) -> Self {
        fs::create_dir_all(root).unwrap();
        install_fake_pipe_capture(root);
        Self {
            root: root.to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn start_capture(&self, pane: &str, command: &str) -> CaptureIdentity {
        let status = Command::new(self.root.join("fake-pipe-capture"))
            .arg("start")
            .arg(&self.root)
            .arg(pane)
            .arg(command)
            .status()
            .unwrap();
        assert!(status.success(), "fake capture helper did not start");
        wait_for_capture_identity(&self.root, pane)
    }

    pub fn stop_capture(&self, pane: &str) -> bool {
        Command::new(self.root.join("fake-pipe-capture"))
            .arg("stop")
            .arg(&self.root)
            .arg(pane)
            .status()
            .is_ok_and(|status| status.success())
    }

    pub fn capture_identity(&self, pane: &str) -> Option<CaptureIdentity> {
        read_capture_identity(&self.root, pane)
    }

    pub fn wait_for_hooks(&self) -> bool {
        wait_for_fake_hook_helpers(&self.root)
    }

    pub fn stop_all(&self) -> bool {
        stop_and_wait_fake_tmux_helpers(&self.root)
    }
}

impl Drop for ControlledTmuxFixture {
    fn drop(&mut self) {
        let _ = self.stop_all();
    }
}

pub fn install_fake_pipe_capture(root: &Path) {
    let path = root.join("fake-pipe-capture");
    fs::write(
        &path,
        r#"#!/bin/sh
mode="$1"
root="$2"
pane="$3"
fifo="$root/pipe-$pane.fifo"
stop="$root/pipe-$pane.stop"
done="$root/pipe-$pane.done"
pids="$root/pipe-$pane.pids"
owner="$root/pipe-$pane.owner"
proc_start_time() {
  test -r "/proc/$1/stat" || return 1
  awk '{print $22}' "/proc/$1/stat" 2>/dev/null
}
identity_matches() {
  test -n "$1" && test -n "$2" || return 1
  actual="$(proc_start_time "$1")" || return 1
  test "$actual" = "$2"
}
load_child_identities() {
  recorded_nonce=''
  sink_pid=''
  sink_start=''
  writer_pid=''
  writer_start=''
  test -f "$pids" || return 0
  while read -r kind first second; do
    case "$kind" in
      nonce) recorded_nonce="$first" ;;
      sink) sink_pid="$first"; sink_start="$second" ;;
      writer) writer_pid="$first"; writer_start="$second" ;;
      *) return 1 ;;
    esac
  done < "$pids"
  test -z "$recorded_nonce" || test "$recorded_nonce" = "$nonce"
}
children_gone() {
  ! identity_matches "$writer_pid" "$writer_start" &&
    ! identity_matches "$sink_pid" "$sink_start"
}
wait_children_gone() {
  attempts=0
  while ! children_gone; do
    attempts=$((attempts + 1))
    if test "$attempts" -ge 100; then return 1; fi
    sleep 0.02
  done
}
signal_if_owned() {
  signal="$1"
  pid="$2"
  start="$3"
  if identity_matches "$pid" "$start"; then
    kill "-$signal" "$pid" 2>/dev/null || true
  fi
}
stop_stale_capture() {
  load_child_identities || return 1
  printf '%s\n' "$nonce" > "$stop.tmp.$$"
  mv "$stop.tmp.$$" "$stop"
  if ! wait_children_gone; then
    signal_if_owned TERM "$writer_pid" "$writer_start"
    signal_if_owned TERM "$sink_pid" "$sink_start"
    if ! wait_children_gone; then
      signal_if_owned KILL "$writer_pid" "$writer_start"
      signal_if_owned KILL "$sink_pid" "$sink_start"
      wait_children_gone || return 1
    fi
  fi
  printf '%s %s %s %s\n' "$nonce" 'stale' 'stale' 'supervisor_missing' > "$done.tmp.$$"
  mv "$done.tmp.$$" "$done"
  rm -f "$fifo"
}
case "$mode" in
  start)
    command="$4"
    if test -f "$owner"; then
      read -r owner_nonce owner_pid owner_start < "$owner"
      if identity_matches "$owner_pid" "$owner_start"; then exit 0; fi
      "$0" stop "$root" "$pane" || exit 1
    fi
    nonce="$pane-$$-$(date +%s%N)"
    rm -f "$fifo" "$stop" "$done" "$pids" "$owner"
    mkfifo "$fifo"
    "$0" supervise "$root" "$pane" "$nonce" "$command" \
      </dev/null >> "$root/pipe-supervisor.out" 2>> "$root/pipe-supervisor.err" &
    supervisor_pid=$!
    supervisor_start="$(proc_start_time "$supervisor_pid")" || exit 1
    printf '%s %s %s\n' "$nonce" "$supervisor_pid" "$supervisor_start" > "$owner.tmp.$$"
    mv "$owner.tmp.$$" "$owner"
    attempts=0
    while test ! -f "$pids"; do
      attempts=$((attempts + 1))
      if test "$attempts" -ge 250; then exit 1; fi
      sleep 0.02
    done
    ;;
  supervise)
    nonce="$4"
    command="$5"
    attempts=0
    while test ! -f "$owner" || ! grep -Fq "$nonce " "$owner"; do
      attempts=$((attempts + 1))
      if test "$attempts" -ge 250; then exit 1; fi
      sleep 0.02
    done
    sh -c "$command" < "$fifo" >> "$root/pipe-helper.out" 2>> "$root/pipe-helper.err" &
    sink_pid=$!
    (
      printf 'capture online for %s\n' "$pane"
      while ! test -f "$stop" || ! grep -Fqx "$nonce" "$stop"; do sleep 0.02; done
    ) > "$fifo" 2>/dev/null &
    writer_pid=$!
    sink_start="$(proc_start_time "$sink_pid")" || exit 1
    writer_start="$(proc_start_time "$writer_pid")" || exit 1
    {
      printf 'nonce %s\n' "$nonce"
      printf 'sink %s %s\n' "$sink_pid" "$sink_start"
      printf 'writer %s %s\n' "$writer_pid" "$writer_start"
    } > "$pids.tmp.$$"
    mv "$pids.tmp.$$" "$pids"
    wait "$writer_pid"
    writer_status=$?
    wait "$sink_pid"
    sink_status=$?
    printf '%s %s %s\n' "$nonce" "$sink_status" "$writer_status" > "$done.tmp.$$"
    mv "$done.tmp.$$" "$done"
    rm -f "$fifo"
    ;;
  stop)
    test -f "$owner" || exit 0
    read -r nonce supervisor_pid supervisor_start < "$owner"
    if ! identity_matches "$supervisor_pid" "$supervisor_start"; then
      stop_stale_capture
      exit $?
    fi
    printf '%s\n' "$nonce" > "$stop.tmp.$$"
    mv "$stop.tmp.$$" "$stop"
    attempts=0
    while test ! -f "$done" || ! grep -Fq "$nonce " "$done"; do
      attempts=$((attempts + 1))
      if test "$attempts" -ge 250; then exit 1; fi
      if ! identity_matches "$supervisor_pid" "$supervisor_start"; then
        stop_stale_capture
        exit $?
      fi
      sleep 0.02
    done
    ;;
  *)
    exit 2
    ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

pub fn wait_for_fake_hook_helpers(root: &Path) -> bool {
    let started = Instant::now();
    let mut quiet_polls = 0;
    while started.elapsed() < Duration::from_secs(30) {
        if !fake_hook_helper_pending(root) {
            quiet_polls += 1;
            if quiet_polls >= 2 {
                return true;
            }
        } else {
            quiet_polls = 0;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

fn fake_hook_helper_pending(root: &Path) -> bool {
    fs::read_dir(root).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if !file_name.starts_with("hook-helper-") {
                return false;
            }
            if entry.path().extension().is_some_and(|ext| ext == "pending") {
                return true;
            }
            if entry.path().extension().is_some_and(|ext| ext == "pid") {
                return fs::read_to_string(entry.path())
                    .ok()
                    .and_then(|pid| pid.trim().parse::<i32>().ok())
                    .is_some_and(process_exists);
            }
            false
        })
    })
}

fn process_exists(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    Path::new("/proc").join(pid.to_string()).exists()
}

pub fn stop_and_wait_fake_tmux_helpers(root: &Path) -> bool {
    let helper = root.join("fake-pipe-capture");
    let panes = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_prefix("pipe-")
                .and_then(|name| name.strip_suffix(".owner"))
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    let captures_stopped = panes.into_iter().all(|pane| {
        Command::new(&helper)
            .arg("stop")
            .arg(root)
            .arg(pane)
            .status()
            .is_ok_and(|status| status.success())
    });
    captures_stopped && wait_for_fake_hook_helpers(root)
}

pub fn read_capture_identity(root: &Path, pane: &str) -> Option<CaptureIdentity> {
    let owner = fs::read_to_string(root.join(format!("pipe-{pane}.owner"))).ok()?;
    let mut owner = owner.split_whitespace();
    let nonce = owner.next()?.to_string();
    let supervisor = ProcessIdentity {
        pid: owner.next()?.parse().ok()?,
        start_time: owner.next()?.parse().ok()?,
    };
    let pids = fs::read_to_string(root.join(format!("pipe-{pane}.pids"))).ok()?;
    let mut sink = None;
    let mut writer = None;
    for line in pids.lines() {
        let mut fields = line.split_whitespace();
        match fields.next()? {
            "nonce" if fields.next()? == nonce => {}
            "sink" => {
                sink = Some(ProcessIdentity {
                    pid: fields.next()?.parse().ok()?,
                    start_time: fields.next()?.parse().ok()?,
                });
            }
            "writer" => {
                writer = Some(ProcessIdentity {
                    pid: fields.next()?.parse().ok()?,
                    start_time: fields.next()?.parse().ok()?,
                });
            }
            _ => return None,
        }
    }
    Some(CaptureIdentity {
        nonce,
        supervisor,
        sink: sink?,
        writer: writer?,
    })
}

pub fn process_identity_is_alive(identity: &ProcessIdentity) -> bool {
    process_start_time(identity.pid).is_some_and(|start_time| start_time == identity.start_time)
}

pub fn capture_identity_is_alive(identity: &CaptureIdentity) -> bool {
    process_identity_is_alive(&identity.supervisor)
        && process_identity_is_alive(&identity.sink)
        && process_identity_is_alive(&identity.writer)
}

fn wait_for_capture_identity(root: &Path, pane: &str) -> CaptureIdentity {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if let Some(identity) = read_capture_identity(root, pane)
            && capture_identity_is_alive(&identity)
        {
            return identity;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("fake capture identity was not ready for pane {pane}");
}

fn process_start_time(pid: i32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(") ")?;
    fields.split_whitespace().nth(19)?.parse().ok()
}

pub fn fake_tmux_with_sequential_panes(control: &ControlledTmuxFixture) -> PathBuf {
    let root = control.root();
    let path = root.join("fake-tmux-sequential-panes");
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
record_pane() {{
  printf '%s\t%s\t%s\t%s\t%s\n' 'host-a' '%7' 'flow-a' "$1" "$2" >> "$root/panes"
}}
handle_input() {{
  input="$1"
  case "$input" in
    *humanize-plugin-driver*--run-id*)
      pane="${{target##*.}}"
      TMUX_PANE="$pane" sh -c "$input" >> "$root/driver.out" 2>> "$root/driver.err" &
      printf '%s\n' "$!" > "$root/driver.pid"
      printf '%s\n' "$!" > "$root/driver-${{pane#%}}.pid"
      ;;
    *humanize-test-agent*)
      if test -n "$HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT"; then
        : > "$HUMANIZE_TEST_DRIVER_EVENT_FAULT_AFTER_AGENT"
      fi
      load_ready_environment "$input"
      pane="${{target##*.}}"
      pending="$root/hook-helper-${{pane#%}}-$$.pending"
      done="${{pending%.pending}}.done"
      pidfile="${{pending%.pending}}.pid"
      : > "$pending"
      (
        printf '{{"hook_event_name":"SessionStart","session_id":"fake-native-%s"}}\n' "${{pane#%}}" |
          HUMANIZE_RUNS_DIR="$root/runs" TMUX_PANE="$pane" '{}' --agent-ready-hook --source codex_session_start
        mv "$pending" "$done"
        rm -f "$pidfile"
      ) </dev/null >> "$root/hook.out" 2>> "$root/hook.err" &
      printf '%s\n' "$!" > "$pidfile"
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
    printf '%s\n' "$pane_id" >> "$root/driver-panes"
    record_pane "$pane_id" "$last"
    printf '%s\t%s\n' '%7' "$pane_id"
    ;;
  split-window)
    pane="$(cat "$root/pane.counter")"
    pane="$((pane + 1))"
    printf '%s\n' "$pane" > "$root/pane.counter"
    pane_id="%$pane"
    record_pane "$pane_id" "$last"
    printf '%s\n' "$pane_id"
    ;;
  list-panes)
    test -f "$root/panes" && cat "$root/panes"
    ;;
  display-message)
    if test -f "$root/killed-panes" && grep -Fqx "$target" "$root/killed-panes"; then exit 42; fi
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
    if test -n "$HUMANIZE_TEST_FAIL_KILL_PANE" && test "$target" = "$HUMANIZE_TEST_FAIL_KILL_PANE"; then
      exit 43
    fi
    printf '%s\n' "$target" >> "$root/killed-panes"
    pane="${{target##*.}}"
    if test -f "$root/panes"; then
      awk -F '\t' -v pane="$pane" '$4 != pane' "$root/panes" > "$root/panes.next"
      mv "$root/panes.next" "$root/panes"
    fi
    if test -f "$root/driver-panes" && grep -Fqx "$pane" "$root/driver-panes"; then
      if test -f "$root/driver-${{pane#%}}.pid"; then
        kill "$(cat "$root/driver-${{pane#%}}.pid")" 2>/dev/null || true
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
