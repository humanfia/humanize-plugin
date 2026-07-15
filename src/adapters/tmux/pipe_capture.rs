use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::pipe_sink::{
    PipeSinkIdentity, PipeSinkReady, verify_pipe_sink_ready_ack_under_root,
    verify_pipe_sink_ready_ack_under_root_preserve,
};

use super::{TmuxError, TmuxPipeCapture, argv};

pub(crate) struct PipeCaptureRequest<'a> {
    pub(crate) root: &'a Path,
    pub(crate) transcript_relative_path: &'a Path,
    pub(crate) identity: &'a PipeSinkIdentity,
    pub(crate) ack_relative_path: &'a Path,
    pub(crate) completion_relative_path: &'a Path,
    pub(crate) ack_nonce: &'a str,
    pub(crate) preserve_ready_ack: bool,
}

pub(super) fn shell_single_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    shell_single_quote_str(&value)
}

pub(super) fn shell_single_quote_str(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) fn wait_for_pipe_ack(
    request: &PipeCaptureRequest<'_>,
    sink: &Path,
    argv: &[String],
    timeout: Duration,
) -> Result<PipeSinkReady, TmuxError> {
    let deadline = Instant::now() + timeout;
    loop {
        let verification = if request.preserve_ready_ack {
            verify_pipe_sink_ready_ack_under_root_preserve(
                request.root,
                request.ack_relative_path,
                request.ack_nonce,
                request.identity,
                sink,
            )
        } else {
            verify_pipe_sink_ready_ack_under_root(
                request.root,
                request.ack_relative_path,
                request.ack_nonce,
                request.identity,
                sink,
            )
        };
        match verification {
            Ok(ready) => return Ok(ready),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_err) => {
                return Err(TmuxError::io(argv, "pipe sink setup failed"));
            }
        }
        if Instant::now() >= deadline {
            return Err(TmuxError::io(
                argv,
                &format!(
                    "pipe sink did not acknowledge readiness: {}",
                    request.root.join(request.ack_relative_path).display()
                ),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn default_pipe_completion_path(ack_relative_path: &Path) -> PathBuf {
    let file_name = ack_relative_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("pipe.ready");
    ack_relative_path.with_file_name(format!("{file_name}.complete"))
}

pub(super) fn pipe_completion_error(argv: &[String]) -> TmuxError {
    TmuxError::io(argv, "pipe sink completion failed")
}

pub(super) fn wait_for_pipe_helper_exit(
    capture: &TmuxPipeCapture,
    deadline: Instant,
) -> std::io::Result<()> {
    if !capture.external_helper {
        return Ok(());
    }
    while helper_process_matches(capture.helper_pid, capture.helper_process_start_time_ticks)? {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "pipe sink completion failed",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[cfg(all(unix, target_os = "linux"))]
pub(super) fn helper_process_matches(
    pid: u32,
    expected_start_time_ticks: u64,
) -> std::io::Result<bool> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    let fields = stat
        .rsplit_once(')')
        .map(|(_, fields)| fields)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "pipe sink process identity verification failed",
            )
        })?;
    let actual_start_time_ticks = fields
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "pipe sink process identity verification failed",
            )
        })?
        .parse::<u64>()
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "pipe sink process identity verification failed",
            )
        })?;
    Ok(actual_start_time_ticks == expected_start_time_ticks)
}

#[cfg(not(all(unix, target_os = "linux")))]
pub(super) fn helper_process_matches(_: u32, _: u64) -> std::io::Result<bool> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "durable tmux transcript capture is not supported on this platform",
    ))
}

pub(super) fn pipe_sink_redacted_argv(target: &str) -> Vec<String> {
    argv(
        ["tmux", "pipe-pane", "-o", "-t", target],
        ["<pipe-sink-command-redacted>"],
    )
}

pub(super) fn redact_pipe_sink_error(err: TmuxError, redacted_argv: &[String]) -> TmuxError {
    match err {
        TmuxError::Io { .. } => TmuxError::io(redacted_argv, "pipe sink setup failed"),
        TmuxError::CommandFailed { status, .. } => {
            TmuxError::command_failed(redacted_argv, status, "pipe sink setup failed")
        }
        other => other,
    }
}

pub(super) fn pipe_ack_nonce() -> String {
    let mut bytes = [0_u8; 16];
    if File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok()
    {
        return bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    }
    let fallback = format!(
        "{}-{}-{:?}",
        std::process::id(),
        Instant::now().elapsed().as_nanos(),
        std::thread::current().id()
    );
    fallback
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}
