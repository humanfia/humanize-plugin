use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use super::persistence::{
    read_driver_events, read_runtime_events, read_runtime_referenced_locks, replay_driver_events,
};
use super::{private_driver_dir, runtime_root_for_run_root};

const ATTACH_LOCK_FILE: &str = "attach.lock";
const ATTACH_LOCK_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DriverRecoveryTmux {
    pub session_id: String,
    pub window_name: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DriverRecoveryState {
    pub tmux: Option<DriverRecoveryTmux>,
}

pub struct DriverAttachLock {
    file: File,
}

impl Drop for DriverAttachLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub fn load_driver_recovery_state(
    run_root: &Path,
    expected_run_id: &str,
) -> io::Result<Option<DriverRecoveryState>> {
    validate_private_run_id(run_root, expected_run_id)?;
    let private_run_root = private_run_root_for_public_run_root(run_root)?;
    let runtime_events = read_runtime_events(&private_run_root)?;
    let runtime_locks = read_runtime_referenced_locks(&private_run_root, &runtime_events)?;
    let runtime = crate::runtime::Runtime::from_events(runtime_events);
    let state = runtime.state();
    let Some(lock_id) = state.flow_lock_id_by_run.get(expected_run_id) else {
        return Ok(None);
    };
    let Some(content_hash) = state.contract_hash_by_run.get(expected_run_id) else {
        return Ok(None);
    };
    let Some(package) = runtime_locks.get(lock_id) else {
        return Ok(None);
    };
    if package.content_hash() != content_hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "durable runtime binding does not match its immutable flow lock",
        ));
    }
    package
        .lock()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.message))?;
    let events = read_driver_events(&private_run_root)?;
    let replay = replay_driver_events(&private_run_root, &events)?;
    let tmux = replay.tmux.map(|tmux| DriverRecoveryTmux {
        session_id: tmux.session_id,
        window_name: tmux.window_name,
    });
    Ok(Some(DriverRecoveryState { tmux }))
}

pub fn acquire_driver_attach_lock(run_root: &Path) -> io::Result<DriverAttachLock> {
    let runtime_root = runtime_root_for_run_root(run_root)?;
    let private_run_root = crate::private_state::ensure_private_run_root(&runtime_root, run_root)?;
    let driver_dir = private_run_root.join("driver");
    crate::private_state::ensure_private_directory(&driver_dir)?;
    let lock_path = driver_dir.join(ATTACH_LOCK_FILE);
    let file = crate::run_assets::open_private_lock_file(&lock_path)
        .map_err(|err| io::Error::other(err.to_string()))?;
    let started = Instant::now();
    loop {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(DriverAttachLock { file });
        }
        let err = io::Error::last_os_error();
        if !matches!(
            err.raw_os_error(),
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN
        ) {
            return Err(err);
        }
        if started.elapsed() >= ATTACH_LOCK_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "driver attach lock deadline exceeded",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn private_run_root_for_public_run_root(run_root: &Path) -> io::Result<std::path::PathBuf> {
    let driver_dir = private_driver_dir_for_public_run_root(run_root)?;
    driver_dir.parent().map(Path::to_path_buf).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "private driver dir has no parent",
        )
    })
}

fn private_driver_dir_for_public_run_root(run_root: &Path) -> io::Result<std::path::PathBuf> {
    let runtime_root = runtime_root_for_run_root(run_root)?;
    Ok(private_driver_dir(&runtime_root, run_root))
}

fn validate_private_run_id(run_root: &Path, expected_run_id: &str) -> io::Result<()> {
    let runtime_root = runtime_root_for_run_root(run_root)?;
    let identity =
        crate::private_state::read_run_identity(&runtime_root, run_root)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "private run identity is missing")
        })?;
    if identity.run_id != expected_run_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private run identity does not match the requested run",
        ));
    }
    Ok(())
}
