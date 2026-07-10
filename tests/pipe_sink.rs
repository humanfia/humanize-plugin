use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::thread;

use humanize_plugin::pipe_sink::{
    PipeSinkAckPayload, PipeSinkAckRequest, PipeSinkCompletionPayload, PipeSinkIdentity,
    append_reader_to_pipe_log, append_reader_to_pipe_log_under_root, pipe_sink_identity,
    verify_pipe_sink_ack_under_root, verify_pipe_sink_completion_under_root,
    verify_pipe_sink_ready_ack_under_root,
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(all(unix, target_os = "linux"))]
use std::process::Command;
#[cfg(unix)]
use std::time::{Duration, Instant};

fn test_temp_dir(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(name);
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    fs::create_dir_all(&path).unwrap();
    path
}

#[cfg(unix)]
fn create_fifo(path: &std::path::Path) -> PipeSinkIdentity {
    let fifo_c = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: mkfifo receives a valid nul-terminated filesystem path.
    let result = unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) };
    assert_eq!(result, 0);
    let metadata = fs::symlink_metadata(path).unwrap();
    PipeSinkIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        uid: metadata.uid(),
        mode: metadata.mode() & 0o777,
        nlink: metadata.nlink(),
    }
}

#[cfg(unix)]
fn open_fifo_read_write_nonblocking(path: &std::path::Path) -> fs::File {
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK);
    options.open(path).unwrap()
}

#[test]
fn pipe_sink_appends_reader_to_private_log_file() {
    let root = test_temp_dir("pipe-sink-append");
    let path = root.join("transcript.pipe.log");

    append_reader_to_pipe_log(&path, &mut Cursor::new("first\n")).unwrap();
    append_reader_to_pipe_log(&path, &mut Cursor::new("second\n")).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "first\nsecond\n");
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn pipe_sink_rejects_symlink_destination() {
    let root = test_temp_dir("pipe-sink-symlink");
    let outside = test_temp_dir("pipe-sink-symlink-outside");
    let path = root.join("transcript.pipe.log");
    symlink(outside.join("target.log"), &path).unwrap();

    let result = append_reader_to_pipe_log(&path, &mut Cursor::new("secret\n"));

    assert!(result.is_err());
    assert!(!outside.join("target.log").exists());
}

#[cfg(unix)]
#[test]
fn pipe_sink_rejects_fifo_transcript_without_blocking() {
    let root = test_temp_dir("pipe-sink-transcript-fifo");
    let path = root.join("transcript.pipe.log");
    create_fifo(&path);
    let _keepalive = open_fifo_read_write_nonblocking(&path);

    let started = Instant::now();
    let result = append_reader_to_pipe_log(&path, &mut Cursor::new("body\n"));

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(unix)]
#[test]
fn pipe_sink_identity_rejects_fifo_without_blocking() {
    let root = test_temp_dir("pipe-sink-identity-fifo");
    let path = root.join("transcript.pipe.log");
    create_fifo(&path);
    let _keepalive = open_fifo_read_write_nonblocking(&path);

    let started = Instant::now();
    let result = pipe_sink_identity(&path);

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn pipe_sink_rejects_absolute_or_parent_relative_paths() {
    let root = test_temp_dir("pipe-sink-contained-paths");
    let path = root.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let identity = pipe_sink_identity(&path).unwrap();

    let absolute = append_reader_to_pipe_log_under_root(
        &root,
        &path,
        &identity,
        None,
        &mut Cursor::new("absolute\n"),
    );
    let parent = append_reader_to_pipe_log_under_root(
        &root,
        "../transcript.pipe.log",
        &identity,
        None,
        &mut Cursor::new("parent\n"),
    );

    assert!(absolute.is_err());
    assert!(parent.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "");
}

#[cfg(unix)]
#[test]
fn pipe_sink_rejects_parent_symlink_swap_and_does_not_ack() {
    let root = test_temp_dir("pipe-sink-parent-swap");
    let outside = test_temp_dir("pipe-sink-parent-swap-outside");
    let activation_dir = root.join("activations/root");
    fs::create_dir_all(&activation_dir).unwrap();
    let path = activation_dir.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let identity = pipe_sink_identity(&path).unwrap();
    fs::remove_dir_all(root.join("activations")).unwrap();
    symlink(&outside, root.join("activations")).unwrap();

    let result = append_reader_to_pipe_log_under_root(
        &root,
        "activations/root/transcript.pipe.log",
        &identity,
        Some(&PipeSinkAckRequest::new(
            "activations/root/pipe.ready",
            "nonce-a",
        )),
        &mut Cursor::new("swapped\n"),
    );

    assert!(result.is_err());
    assert!(!outside.join("root/transcript.pipe.log").exists());
    assert!(!outside.join("root/pipe.ready").exists());
}

#[cfg(unix)]
#[test]
fn pipe_sink_rejects_hard_linked_transcript() {
    let root = test_temp_dir("pipe-sink-hard-link");
    let activation_dir = root.join("activations/root");
    fs::create_dir_all(&activation_dir).unwrap();
    let path = activation_dir.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let identity = pipe_sink_identity(&path).unwrap();
    fs::hard_link(&path, root.join("extra-link.log")).unwrap();

    let result = append_reader_to_pipe_log_under_root(
        &root,
        "activations/root/transcript.pipe.log",
        &identity,
        Some(&PipeSinkAckRequest::new(
            "activations/root/pipe.ready",
            "nonce-a",
        )),
        &mut Cursor::new("linked\n"),
    );

    assert!(result.is_err());
    assert!(!root.join("activations/root/pipe.ready").exists());
}

#[test]
fn pipe_sink_writes_ready_ack_after_verified_open() {
    let root = test_temp_dir("pipe-sink-ready");
    let activation_dir = root.join("activations/root");
    fs::create_dir_all(&activation_dir).unwrap();
    let path = activation_dir.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let identity = pipe_sink_identity(&path).unwrap();

    append_reader_to_pipe_log_under_root(
        &root,
        "activations/root/transcript.pipe.log",
        &identity,
        Some(&PipeSinkAckRequest::new(
            "activations/root/pipe.ready",
            "nonce-a",
        )),
        &mut Cursor::new("ready\n"),
    )
    .unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "ready\n");
    let ack: PipeSinkAckPayload = serde_json::from_str(
        &fs::read_to_string(root.join("activations/root/pipe.ready")).unwrap(),
    )
    .unwrap();
    assert_eq!(ack.nonce, "nonce-a");
    assert_eq!(ack.transcript_dev, identity.dev);
    assert_eq!(ack.transcript_ino, identity.ino);
}

#[test]
fn pipe_sink_rejects_identity_mismatch() {
    let root = test_temp_dir("pipe-sink-identity-mismatch");
    let path = root.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let mut identity = pipe_sink_identity(&path).unwrap();
    identity.ino = identity.ino.wrapping_add(1);

    let result = append_reader_to_pipe_log_under_root(
        &root,
        "transcript.pipe.log",
        &identity,
        Some(&PipeSinkAckRequest::new("pipe.ready", "nonce-a")),
        &mut Cursor::new("mismatch\n"),
    );

    assert!(result.is_err());
    assert!(!root.join("pipe.ready").exists());
}

#[test]
fn pipe_sink_ack_verification_rejects_wrong_transcript_inode() {
    let root = test_temp_dir("pipe-sink-ack-wrong-inode");
    let path = root.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let identity = pipe_sink_identity(&path).unwrap();
    let ack = PipeSinkAckPayload {
        nonce: "nonce-a".to_string(),
        pid: std::process::id(),
        transcript_dev: identity.dev,
        transcript_ino: identity.ino.wrapping_add(1),
    };
    fs::write(
        root.join("pipe.ready"),
        serde_json::to_string(&ack).unwrap(),
    )
    .unwrap();

    let result = verify_pipe_sink_ack_under_root(
        &root,
        "pipe.ready",
        "nonce-a",
        &identity,
        &std::env::current_exe().unwrap(),
    );

    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn pipe_sink_ack_verification_rejects_symlink_ack_file() {
    let root = test_temp_dir("pipe-sink-ack-symlink");
    let outside = test_temp_dir("pipe-sink-ack-symlink-outside");
    let path = root.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let identity = pipe_sink_identity(&path).unwrap();
    fs::write(outside.join("ack"), "{}").unwrap();
    symlink(outside.join("ack"), root.join("pipe.ready")).unwrap();

    let result = verify_pipe_sink_ack_under_root(
        &root,
        "pipe.ready",
        "nonce-a",
        &identity,
        &std::env::current_exe().unwrap(),
    );

    assert!(result.is_err());
}

#[test]
fn pipe_sink_ack_verification_rejects_partial_ack_data() {
    let root = test_temp_dir("pipe-sink-ack-partial");
    let path = root.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let identity = pipe_sink_identity(&path).unwrap();
    fs::write(root.join("pipe.ready"), "{\"nonce\":\"nonce-a\"").unwrap();

    let result = verify_pipe_sink_ack_under_root(
        &root,
        "pipe.ready",
        "nonce-a",
        &identity,
        &std::env::current_exe().unwrap(),
    );

    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn pipe_sink_ack_verification_rejects_fifo_without_blocking() {
    let root = test_temp_dir("pipe-sink-ack-fifo");
    let path = root.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let identity = pipe_sink_identity(&path).unwrap();
    let fifo_path = root.join("pipe.ready");
    let fifo_c = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: mkfifo receives a valid nul-terminated filesystem path.
    let result = unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) };
    assert_eq!(result, 0);

    let started = Instant::now();
    let result = verify_pipe_sink_ack_under_root(
        &root,
        "pipe.ready",
        "nonce-a",
        &identity,
        &std::env::current_exe().unwrap(),
    );

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(all(unix, target_os = "linux"))]
#[test]
fn pipe_sink_process_rejects_fifo_transcript_without_blocking() {
    let root = test_temp_dir("pipe-sink-process-transcript-fifo");
    let activation_dir = root.join("activations/root");
    fs::create_dir_all(&activation_dir).unwrap();
    let transcript_relative = "activations/root/transcript.pipe.log";
    let transcript_path = root.join(transcript_relative);
    let identity = create_fifo(&transcript_path);
    let executable = env!("CARGO_BIN_EXE_humanize-plugin-mcp");
    let mut child = Command::new(executable)
        .args([
            "--pipe-sink",
            "--root",
            root.to_str().unwrap(),
            "--relative",
            transcript_relative,
            "--dev",
            &identity.dev.to_string(),
            "--ino",
            &identity.ino.to_string(),
            "--uid",
            &identity.uid.to_string(),
            "--mode",
            &identity.mode.to_string(),
            "--nlink",
            &identity.nlink.to_string(),
            "--ack-relative",
            "activations/root/pipe.ready",
            "--completion-relative",
            "activations/root/pipe.complete",
            "--ack-nonce",
            "fifo-nonce",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(1) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("pipe sink blocked while opening a FIFO transcript");
        }
        thread::sleep(Duration::from_millis(10));
    };

    assert!(!status.success());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!root.join("activations/root/pipe.ready").exists());
    assert!(!root.join("activations/root/pipe.complete").exists());
}

#[cfg(all(unix, target_os = "linux"))]
#[test]
fn pipe_sink_ack_verification_rejects_pid_zero() {
    let root = test_temp_dir("pipe-sink-ack-pid-zero");
    let path = root.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let identity = pipe_sink_identity(&path).unwrap();
    let ack = PipeSinkAckPayload {
        nonce: "nonce-a".to_string(),
        pid: 0,
        transcript_dev: identity.dev,
        transcript_ino: identity.ino,
    };
    fs::write(
        root.join("pipe.ready"),
        serde_json::to_string(&ack).unwrap(),
    )
    .unwrap();

    let result = verify_pipe_sink_ack_under_root(
        &root,
        "pipe.ready",
        "nonce-a",
        &identity,
        &std::env::current_exe().unwrap(),
    );

    assert!(result.is_err());
}

#[cfg(all(unix, target_os = "linux"))]
#[test]
fn pipe_sink_ack_verification_rejects_missing_proc_entry() {
    let root = test_temp_dir("pipe-sink-ack-missing-proc");
    let path = root.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let identity = pipe_sink_identity(&path).unwrap();
    let ack = PipeSinkAckPayload {
        nonce: "nonce-a".to_string(),
        pid: u32::MAX,
        transcript_dev: identity.dev,
        transcript_ino: identity.ino,
    };
    fs::write(
        root.join("pipe.ready"),
        serde_json::to_string(&ack).unwrap(),
    )
    .unwrap();

    let result = verify_pipe_sink_ack_under_root(
        &root,
        "pipe.ready",
        "nonce-a",
        &identity,
        &std::env::current_exe().unwrap(),
    );

    assert!(result.is_err());
}

#[cfg(all(unix, target_os = "linux"))]
#[test]
fn pipe_sink_ack_verification_rejects_dead_helper() {
    let root = test_temp_dir("pipe-sink-ack-dead-helper");
    let path = root.join("transcript.pipe.log");
    fs::write(&path, "").unwrap();
    let identity = pipe_sink_identity(&path).unwrap();
    let mut child = Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    let ack = PipeSinkAckPayload {
        nonce: "nonce-a".to_string(),
        pid,
        transcript_dev: identity.dev,
        transcript_ino: identity.ino,
    };
    fs::write(
        root.join("pipe.ready"),
        serde_json::to_string(&ack).unwrap(),
    )
    .unwrap();

    let result = verify_pipe_sink_ack_under_root(
        &root,
        "pipe.ready",
        "nonce-a",
        &identity,
        &std::env::current_exe().unwrap(),
    );

    assert!(result.is_err());
}

#[cfg(all(unix, target_os = "linux"))]
#[test]
fn pipe_sink_process_drains_trailing_bytes_and_publishes_durable_completion() {
    let root = test_temp_dir("pipe-sink-process-completion");
    let activation_dir = root.join("activations/root");
    fs::create_dir_all(&activation_dir).unwrap();
    let transcript_relative = "activations/root/transcript.pipe.log";
    let ready_relative = "activations/root/pipe.ready";
    let completion_relative = "activations/root/pipe.complete";
    let transcript_path = root.join(transcript_relative);
    fs::write(&transcript_path, "prefix\n").unwrap();
    let identity = pipe_sink_identity(&transcript_path).unwrap();
    let nonce = "completion-nonce";
    let executable = env!("CARGO_BIN_EXE_humanize-plugin-mcp");
    let mut child = Command::new(executable)
        .args([
            "--pipe-sink",
            "--root",
            root.to_str().unwrap(),
            "--relative",
            transcript_relative,
            "--dev",
            &identity.dev.to_string(),
            "--ino",
            &identity.ino.to_string(),
            "--uid",
            &identity.uid.to_string(),
            "--mode",
            &identity.mode.to_string(),
            "--nlink",
            &identity.nlink.to_string(),
            "--ack-relative",
            ready_relative,
            "--completion-relative",
            completion_relative,
            "--ack-nonce",
            nonce,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    let ready = loop {
        match verify_pipe_sink_ready_ack_under_root(
            &root,
            ready_relative,
            nonce,
            &identity,
            PathBuf::from(executable).as_path(),
        ) {
            Ok(ready) => break ready,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => panic!("ready acknowledgement failed: {err}"),
        }
        assert!(Instant::now() < deadline, "ready acknowledgement timed out");
        thread::sleep(Duration::from_millis(10));
    };

    stdin.write_all(b"body\n").unwrap();
    stdin.write_all(b"trailing bytes\n").unwrap();
    drop(stdin);

    let deadline = Instant::now() + Duration::from_secs(2);
    let completion = loop {
        match verify_pipe_sink_completion_under_root(
            &root,
            completion_relative,
            transcript_relative,
            nonce,
            &identity,
            ready.pid,
            ready.process_start_time_ticks,
        ) {
            Ok(completion) => break completion,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => panic!("completion acknowledgement failed: {err}"),
        }
        assert!(
            Instant::now() < deadline,
            "completion acknowledgement timed out"
        );
        thread::sleep(Duration::from_millis(10));
    };
    let status = child.wait().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();

    assert!(status.success(), "pipe sink failed: {stderr}");
    assert_eq!(completion.pid, ready.pid);
    assert_eq!(completion.bytes_appended, 20);
    assert_eq!(completion.transcript_len, 27);
    assert_eq!(
        fs::read_to_string(&transcript_path).unwrap(),
        "prefix\nbody\ntrailing bytes\n"
    );
}

#[test]
fn pipe_sink_completion_rejects_wrong_helper_or_transcript_identity() {
    let root = test_temp_dir("pipe-sink-completion-identity");
    let transcript_path = root.join("transcript.pipe.log");
    fs::write(&transcript_path, "body\n").unwrap();
    let identity = pipe_sink_identity(&transcript_path).unwrap();
    let completion = PipeSinkCompletionPayload {
        nonce: "nonce-a".to_string(),
        pid: 42,
        process_start_time_ticks: 17,
        transcript_dev: identity.dev,
        transcript_ino: identity.ino.wrapping_add(1),
        initial_len: 0,
        bytes_appended: 5,
        transcript_len: 5,
    };
    fs::write(
        root.join("pipe.complete"),
        serde_json::to_string(&completion).unwrap(),
    )
    .unwrap();

    let result = verify_pipe_sink_completion_under_root(
        &root,
        "pipe.complete",
        "transcript.pipe.log",
        "nonce-a",
        &identity,
        42,
        17,
    );

    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn pipe_sink_completion_rejects_fifo_transcript_without_blocking() {
    let root = test_temp_dir("pipe-sink-completion-transcript-fifo");
    let transcript_path = root.join("transcript.pipe.log");
    let identity = create_fifo(&transcript_path);
    let _keepalive = open_fifo_read_write_nonblocking(&transcript_path);
    let completion = PipeSinkCompletionPayload {
        nonce: "nonce-a".to_string(),
        pid: 42,
        process_start_time_ticks: 17,
        transcript_dev: identity.dev,
        transcript_ino: identity.ino,
        initial_len: 0,
        bytes_appended: 0,
        transcript_len: 0,
    };
    fs::write(
        root.join("pipe.complete"),
        serde_json::to_string(&completion).unwrap(),
    )
    .unwrap();

    let started = Instant::now();
    let result = verify_pipe_sink_completion_under_root(
        &root,
        "pipe.complete",
        "transcript.pipe.log",
        "nonce-a",
        &identity,
        42,
        17,
    );

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}
