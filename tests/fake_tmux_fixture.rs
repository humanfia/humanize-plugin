mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use support::driver_tmux::{
    ControlledTmuxFixture, ProcessIdentity, capture_identity_is_alive, process_identity_is_alive,
    read_capture_identity,
};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

#[test]
fn unwind_drops_capture_owner_and_waits_for_pipe_sink() {
    let root = test_root("unwind");
    let command = pipe_sink_command(&root, "unwind");

    let result = std::panic::catch_unwind(|| {
        let fixture = ControlledTmuxFixture::new(&root);
        let identity = fixture.start_capture("8", &command);
        assert!(capture_identity_is_alive(&identity));
        panic!("exercise fixture unwind");
    });

    assert!(result.is_err());
    let identity = read_capture_identity(&root, "8").unwrap();
    assert!(!capture_identity_is_alive(&identity));
    assert_done_matches(&root, "8", &identity.nonce);
}

#[test]
fn driver_sigkill_preserves_capture_and_restart_reuses_it_until_final_stop() {
    let root = test_root("driver-crash");
    let fixture = ControlledTmuxFixture::new(&root);
    let command = pipe_sink_command(&root, "driver-crash");
    let first = fixture.start_capture("8", &command);
    let mut driver = Command::new("sleep").arg("60").spawn().unwrap();

    unsafe {
        libc::kill(driver.id() as i32, libc::SIGKILL);
    }
    driver.wait().unwrap();

    assert!(capture_identity_is_alive(&first));
    let replayed = fixture.start_capture("8", &command);
    assert_eq!(replayed, first);
    assert!(fixture.stop_capture("8"));
    assert!(!capture_identity_is_alive(&first));
    assert_done_matches(&root, "8", &first.nonce);
}

#[test]
fn fixture_drop_alone_stops_owned_capture() {
    let root = test_root("drop-only");
    let identity = {
        let fixture = ControlledTmuxFixture::new(&root);
        fixture.start_capture("8", &pipe_sink_command(&root, "drop-only"))
    };

    assert!(!capture_identity_is_alive(&identity));
    assert_done_matches(&root, "8", &identity.nonce);
}

#[test]
fn dropping_one_fixture_does_not_stop_another_root() {
    let root_a = test_root("isolated-a");
    let root_b = test_root("isolated-b");
    let fixture_a = ControlledTmuxFixture::new(&root_a);
    let fixture_b = ControlledTmuxFixture::new(&root_b);
    let identity_a = fixture_a.start_capture("8", &pipe_sink_command(&root_a, "isolated-a"));
    let identity_b = fixture_b.start_capture("8", &pipe_sink_command(&root_b, "isolated-b"));

    drop(fixture_a);

    assert!(!capture_identity_is_alive(&identity_a));
    assert!(capture_identity_is_alive(&identity_b));
    drop(fixture_b);
    assert!(!capture_identity_is_alive(&identity_b));
}

#[test]
fn stale_pid_with_mismatched_start_time_is_not_treated_as_owned_capture() {
    let root = test_root("pid-reuse");
    let fixture = ControlledTmuxFixture::new(&root);
    let current = ProcessIdentity {
        pid: std::process::id() as i32,
        start_time: u64::MAX,
    };
    fs::write(
        root.join("pipe-8.owner"),
        format!("forged {} {}\n", current.pid, current.start_time),
    )
    .unwrap();

    let identity = fixture.start_capture("8", &pipe_sink_command(&root, "pid-reuse"));

    assert_ne!(identity.supervisor, current);
    assert!(capture_identity_is_alive(&identity));
    assert!(unsafe { libc::kill(current.pid, 0) } == 0);
}

#[test]
fn supervisor_sigkill_reaps_verified_children_before_restarting_capture() {
    let root = test_root("supervisor-crash");
    let fixture = ControlledTmuxFixture::new(&root);
    let command = pipe_sink_command(&root, "supervisor-crash");
    let first = fixture.start_capture("8", &command);

    unsafe {
        libc::kill(first.supervisor.pid, libc::SIGKILL);
    }
    wait_until(|| !process_identity_is_alive(&first.supervisor));
    assert!(process_identity_is_alive(&first.writer));
    assert!(process_identity_is_alive(&first.sink));

    let second =
        fixture.start_capture("8", &pipe_sink_command(&root, "supervisor-crash-restarted"));
    assert_ne!(second.nonce, first.nonce);
    assert!(!process_identity_is_alive(&first.writer));
    assert!(!process_identity_is_alive(&first.sink));
    assert!(capture_identity_is_alive(&second));

    drop(fixture);
    assert!(!capture_identity_is_alive(&second));
    assert!(!process_identity_is_alive(&first.writer));
    assert!(!process_identity_is_alive(&first.sink));
}

fn pipe_sink_command(root: &Path, name: &str) -> String {
    let capture_dir = root.join(format!("capture-{name}"));
    fs::create_dir_all(&capture_dir).unwrap();
    let transcript = capture_dir.join("transcript.log");
    fs::write(&transcript, []).unwrap();
    let mut permissions = fs::metadata(&transcript).unwrap().permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&transcript, permissions).unwrap();
    let metadata = fs::metadata(&transcript).unwrap();
    let relative = transcript.strip_prefix(root).unwrap();
    let ready = capture_dir.join("ready");
    let complete = capture_dir.join("complete");
    format!(
        "{} --pipe-sink --root {} --relative {} --dev {} --ino {} --uid {} --mode {} --nlink {} --ack-relative {} --completion-relative {} --ack-nonce {}",
        shell_quote(Path::new(env!("CARGO_BIN_EXE_humanize-plugin-driver"))),
        shell_quote(root),
        shell_quote(relative),
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
        metadata.mode() & 0o777,
        metadata.nlink(),
        shell_quote(ready.strip_prefix(root).unwrap()),
        shell_quote(complete.strip_prefix(root).unwrap()),
        shell_quote(Path::new(name)),
    )
}

fn assert_done_matches(root: &Path, pane: &str, nonce: &str) {
    let done = fs::read_to_string(root.join(format!("pipe-{pane}.done"))).unwrap();
    assert_eq!(done.split_whitespace().next(), Some(nonce));
}

fn wait_until(predicate: impl Fn() -> bool) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("condition was not satisfied before timeout");
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn test_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir()
        .join("humanize-controlled-tmux")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    root
}
