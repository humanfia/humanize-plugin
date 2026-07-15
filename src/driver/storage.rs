use std::fs;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::run_assets::{
    append_private_line as durable_append_private_line,
    atomic_write_private as durable_atomic_write_private,
    read_regular_private as durable_read_regular_private,
    truncate_private as durable_truncate_private,
};

use super::DriverFailure;
use super::ipc::{IO_TIMEOUT, connect_with_timeout};

pub(super) fn append_json_line_private<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), DriverFailure> {
    append_json_line_private_io(path, value)
        .map_err(|err| DriverFailure::io("persistence_failed", err))
}

pub(super) fn atomic_write_private_json<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), DriverFailure> {
    atomic_write_private_json_io(path, value)
        .map_err(|err| DriverFailure::io("persistence_failed", err))
}

pub(super) fn atomic_write_private_json_io<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    durable_atomic_write_private(path, &bytes).map_err(|err| io::Error::other(err.to_string()))
}

pub(super) fn read_jsonl_recover_torn_tail<T: DeserializeOwned>(path: &Path) -> io::Result<Vec<T>> {
    let Some(mut bytes) =
        durable_read_regular_private(path).map_err(|err| io::Error::other(err.to_string()))?
    else {
        return Ok(Vec::new());
    };
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.ends_with(b"\n") {
        let durable_len = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        durable_truncate_private(path, durable_len as u64)
            .map_err(|err| io::Error::other(err.to_string()))?;
        bytes.truncate(durable_len);
    }
    let content =
        String::from_utf8(bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let mut records = Vec::new();
    for line in content.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        records.push(
            serde_json::from_str::<T>(line)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
        );
    }
    Ok(records)
}

fn append_json_line_private_io<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let mut record = serde_json::to_vec(value).map_err(io::Error::other)?;
    record.push(b'\n');
    durable_append_private_line(path, &record).map_err(|err| io::Error::other(err.to_string()))
}

pub(super) fn remove_regular_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

pub(super) fn contained_relative_path(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "driver event path is outside run root",
        ));
    }
    Ok(root.join(relative_path))
}

pub(super) fn safe_file_segment(value: &str) -> String {
    let mut segment = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if segment.is_empty() {
        segment.push_str("id");
    }
    segment
}

pub(super) fn validate_run_id(run_id: &str) -> io::Result<()> {
    if run_id.is_empty()
        || run_id
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "run_id must be non-empty and must not contain path separators",
        ));
    }
    Ok(())
}

pub(super) fn remove_stale_socket(path: &Path, runtime_root: &Path) -> io::Result<()> {
    if !path.starts_with(runtime_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path is outside runtime root",
        ));
    }
    match connect_with_timeout(path, IO_TIMEOUT) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "driver socket is already in use",
        )),
        Err(err)
            if path.exists()
                && matches!(
                    err.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
        {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_socket() {
                fs::remove_file(path)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "stale driver path is not a socket",
                ))
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io::Error::new(
            err.kind(),
            format!("driver socket liveness is indeterminate: {err}"),
        )),
    }
}

pub(super) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::{append_json_line_private_io, read_jsonl_recover_torn_tail};

    #[test]
    fn jsonl_read_append_and_recovery_reject_symlinks_without_mutating_target() {
        for file_name in ["events.jsonl", "driver-events.jsonl"] {
            let root = test_root(&format!("jsonl-symlink-{file_name}"));
            let target = root.join("target.jsonl");
            let path = root.join(file_name);
            fs::write(&target, b"{}\n{").unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            symlink(&target, &path).unwrap();

            assert!(read_jsonl_recover_torn_tail::<serde_json::Value>(&path).is_err());
            assert!(append_json_line_private_io(&path, &json!({ "next": 1 })).is_err());
            assert_eq!(fs::read(&target).unwrap(), b"{}\n{");
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn jsonl_read_and_append_reject_fifo_without_blocking() {
        for file_name in ["events.jsonl", "driver-events.jsonl"] {
            for operation in ["read", "append"] {
                let root = test_root(&format!("jsonl-fifo-{file_name}-{operation}"));
                let path = root.join(file_name);
                let fifo = CString::new(path.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

                let mut child = Command::new(std::env::current_exe().unwrap())
                    .arg("--exact")
                    .arg("driver::storage::tests::jsonl_fifo_child")
                    .env("HUMANIZE_TEST_FIFO_PATH", &path)
                    .env("HUMANIZE_TEST_FIFO_OPERATION", operation)
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
                        panic!("JSONL {operation} blocked on a FIFO");
                    }
                    thread::sleep(Duration::from_millis(10));
                };
                assert!(status.success(), "JSONL {operation} accepted a FIFO");
                fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn jsonl_fifo_child() {
        let Ok(path) = std::env::var("HUMANIZE_TEST_FIFO_PATH") else {
            return;
        };
        let operation = std::env::var("HUMANIZE_TEST_FIFO_OPERATION").unwrap();
        let path = PathBuf::from(path);
        let rejected = match operation.as_str() {
            "read" => read_jsonl_recover_torn_tail::<serde_json::Value>(&path).is_err(),
            "append" => append_json_line_private_io(&path, &json!({ "next": 1 })).is_err(),
            _ => panic!("unknown FIFO operation"),
        };
        assert!(rejected, "JSONL {operation} must reject a FIFO");
    }

    #[test]
    fn jsonl_read_append_and_recovery_reject_public_or_linked_files() {
        for file_name in ["events.jsonl", "driver-events.jsonl"] {
            for (name, linked) in [("public", false), ("linked", true)] {
                let root = test_root(&format!("{name}-{file_name}"));
                let target = root.join("target.jsonl");
                let path = root.join(file_name);
                fs::write(&target, b"{}\n{").unwrap();
                if linked {
                    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
                    fs::hard_link(&target, &path).unwrap();
                } else {
                    fs::rename(&target, &path).unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                }
                let original = fs::read(&path).unwrap();

                assert!(read_jsonl_recover_torn_tail::<serde_json::Value>(&path).is_err());
                assert!(append_json_line_private_io(&path, &json!({ "next": 1 })).is_err());
                assert_eq!(fs::read(&path).unwrap(), original);
                fs::remove_dir_all(root).unwrap();
            }
        }
    }

    fn test_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "humanize-driver-storage-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
}
