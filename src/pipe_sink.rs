use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PipeSinkIdentity {
    pub dev: u64,
    pub ino: u64,
    pub uid: u32,
    pub mode: u32,
    pub nlink: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PipeSinkAckRequest {
    pub relative_path: PathBuf,
    pub nonce: String,
}

impl PipeSinkAckRequest {
    pub fn new(relative_path: impl Into<PathBuf>, nonce: impl Into<String>) -> Self {
        Self {
            relative_path: relative_path.into(),
            nonce: nonce.into(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct PipeSinkAckPayload {
    pub nonce: String,
    pub pid: u32,
    pub transcript_dev: u64,
    pub transcript_ino: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PipeSinkReady {
    pub pid: u32,
    pub process_start_time_ticks: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct PipeSinkCompletionPayload {
    pub nonce: String,
    pub pid: u32,
    pub process_start_time_ticks: u64,
    pub transcript_dev: u64,
    pub transcript_ino: u64,
    pub initial_len: u64,
    pub bytes_appended: u64,
    pub transcript_len: u64,
}

#[cfg(target_os = "linux")]
pub(crate) fn ensure_durable_pipe_capture_supported() -> io::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn ensure_durable_pipe_capture_supported() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable tmux transcript capture is not supported on this platform",
    ))
}

pub fn append_stdin_to_pipe_log(path: &Path) -> io::Result<()> {
    append_reader_to_pipe_log(path, &mut io::stdin().lock())
}

pub fn append_reader_to_pipe_log(path: &Path, reader: &mut impl Read) -> io::Result<()> {
    let mut create_options = OpenOptions::new();
    create_options.create_new(true).append(true);
    #[cfg(unix)]
    {
        create_options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let (mut file, created) = match create_options.open(path) {
        Ok(file) => (file, true),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            let mut existing_options = OpenOptions::new();
            existing_options.append(true);
            #[cfg(unix)]
            existing_options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
            (existing_options.open(path)?, false)
        }
        Err(err) => return Err(err),
    };
    #[cfg(unix)]
    identity_from_file(&file)?;
    #[cfg(not(unix))]
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pipe sink transcript verification failed",
        ));
    }
    #[cfg(unix)]
    {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    if created {
        file.sync_all()?;
        sync_parent_directory(path)?;
    }
    io::copy(reader, &mut file)?;
    file.sync_data()
}

pub fn pipe_sink_identity(path: &Path) -> io::Result<PipeSinkIdentity> {
    #[cfg(unix)]
    {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = options.open(path)?;
        pipe_sink_identity_from_file(&file)
    }
    #[cfg(not(unix))]
    {
        let metadata = std::fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pipe sink transcript verification failed",
            ));
        }
        Ok(PipeSinkIdentity {
            dev: 0,
            ino: 0,
            uid: 0,
            mode: 0,
            nlink: 1,
        })
    }
}

pub(crate) fn pipe_sink_identity_from_file(file: &std::fs::File) -> io::Result<PipeSinkIdentity> {
    #[cfg(unix)]
    {
        identity_from_file(file)
    }
    #[cfg(not(unix))]
    {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pipe sink transcript verification failed",
            ));
        }
        Ok(PipeSinkIdentity {
            dev: 0,
            ino: 0,
            uid: 0,
            mode: 0,
            nlink: 1,
        })
    }
}

pub fn append_reader_to_pipe_log_under_root(
    root: &Path,
    relative_path: impl AsRef<Path>,
    expected: &PipeSinkIdentity,
    ack: Option<&PipeSinkAckRequest>,
    reader: &mut impl Read,
) -> io::Result<()> {
    append_reader_to_pipe_log_under_root_with_completion(
        root,
        relative_path,
        expected,
        ack,
        None,
        reader,
    )
    .map(|_| ())
}

pub fn append_reader_to_pipe_log_under_root_with_completion(
    root: &Path,
    relative_path: impl AsRef<Path>,
    expected: &PipeSinkIdentity,
    ack: Option<&PipeSinkAckRequest>,
    completion: Option<&PipeSinkAckRequest>,
    reader: &mut impl Read,
) -> io::Result<Option<PipeSinkCompletionPayload>> {
    append_reader_to_pipe_log_under_root_with_sync(
        root,
        relative_path.as_ref(),
        expected,
        ack,
        completion,
        reader,
        std::fs::File::sync_all,
    )
}

fn append_reader_to_pipe_log_under_root_with_sync(
    root: &Path,
    relative_path: &Path,
    expected: &PipeSinkIdentity,
    ack: Option<&PipeSinkAckRequest>,
    completion: Option<&PipeSinkAckRequest>,
    reader: &mut impl Read,
    sync: impl FnOnce(&std::fs::File) -> io::Result<()>,
) -> io::Result<Option<PipeSinkCompletionPayload>> {
    ensure_durable_pipe_capture_supported()?;
    reject_uncontained_relative_path(relative_path)?;
    if let Some(ack) = ack {
        reject_uncontained_relative_path(&ack.relative_path)?;
    }
    if let Some(completion) = completion {
        reject_uncontained_relative_path(&completion.relative_path)?;
    }

    let mut file = open_existing_file_beneath(root, relative_path)?;
    let actual = identity_from_file(&file)?;
    verify_identity(expected, &actual)?;
    let initial_len = file.metadata()?.len();
    let process_start_time_ticks = helper_process_start_time_ticks(process::id())?;
    if let Some(ack) = ack {
        write_ready_ack(root, &ack.relative_path, &ack.nonce, &actual)?;
    }
    let bytes_appended = io::copy(reader, &mut file)?;
    sync(&file)?;
    let final_identity = identity_from_file(&file)?;
    verify_identity(expected, &final_identity)?;
    let transcript_len = file.metadata()?.len();
    if initial_len.checked_add(bytes_appended) != Some(transcript_len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pipe sink transcript length verification failed",
        ));
    }
    let payload = completion.map(|completion| PipeSinkCompletionPayload {
        nonce: completion.nonce.clone(),
        pid: process::id(),
        process_start_time_ticks,
        transcript_dev: final_identity.dev,
        transcript_ino: final_identity.ino,
        initial_len,
        bytes_appended,
        transcript_len,
    });
    if let (Some(completion), Some(payload)) = (completion, payload.as_ref()) {
        write_completion_ack(root, &completion.relative_path, payload)?;
    }
    Ok(payload)
}

pub fn verify_pipe_sink_ack_under_root(
    root: &Path,
    relative_path: impl AsRef<Path>,
    expected_nonce: &str,
    expected_transcript: &PipeSinkIdentity,
    expected_exe: &Path,
) -> io::Result<()> {
    verify_pipe_sink_ready_ack_under_root(
        root,
        relative_path,
        expected_nonce,
        expected_transcript,
        expected_exe,
    )
    .map(|_| ())
}

pub fn verify_pipe_sink_ready_ack_under_root(
    root: &Path,
    relative_path: impl AsRef<Path>,
    expected_nonce: &str,
    expected_transcript: &PipeSinkIdentity,
    expected_exe: &Path,
) -> io::Result<PipeSinkReady> {
    ensure_durable_pipe_capture_supported()?;
    let relative_path = relative_path.as_ref();
    reject_uncontained_relative_path(relative_path)?;
    let mut ack_file = open_existing_ack_beneath(root, relative_path)?;
    ensure_regular_file(&ack_file)?;
    let mut payload = String::new();
    ack_file.read_to_string(&mut payload)?;
    let ack: PipeSinkAckPayload = serde_json::from_str(&payload).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("pipe sink acknowledgement verification failed: {err}"),
        )
    })?;
    if ack.nonce != expected_nonce
        || ack.transcript_dev != expected_transcript.dev
        || ack.transcript_ino != expected_transcript.ino
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe sink acknowledgement verification failed",
        ));
    }
    verify_helper_process(&ack, expected_transcript, expected_exe)?;
    let ready = PipeSinkReady {
        pid: ack.pid,
        process_start_time_ticks: helper_process_start_time_ticks(ack.pid)?,
    };
    unlink_file_under_root(root, relative_path)?;
    Ok(ready)
}

pub fn verify_pipe_sink_completion_under_root(
    root: &Path,
    relative_path: impl AsRef<Path>,
    transcript_relative_path: impl AsRef<Path>,
    expected_nonce: &str,
    expected_transcript: &PipeSinkIdentity,
    expected_pid: u32,
    expected_process_start_time_ticks: u64,
) -> io::Result<PipeSinkCompletionPayload> {
    ensure_durable_pipe_capture_supported()?;
    let relative_path = relative_path.as_ref();
    let transcript_relative_path = transcript_relative_path.as_ref();
    reject_uncontained_relative_path(relative_path)?;
    reject_uncontained_relative_path(transcript_relative_path)?;
    let mut ack_file = open_existing_ack_beneath(root, relative_path)?;
    ensure_regular_file(&ack_file)?;
    let mut payload = String::new();
    ack_file.read_to_string(&mut payload)?;
    let completion: PipeSinkCompletionPayload = serde_json::from_str(&payload).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("pipe sink completion verification failed: {err}"),
        )
    })?;
    if completion.nonce != expected_nonce
        || completion.pid != expected_pid
        || completion.process_start_time_ticks != expected_process_start_time_ticks
        || completion.transcript_dev != expected_transcript.dev
        || completion.transcript_ino != expected_transcript.ino
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe sink completion verification failed",
        ));
    }
    let transcript = open_existing_transcript_for_verification(root, transcript_relative_path)?;
    let transcript_identity = identity_from_file(&transcript)?;
    verify_identity(expected_transcript, &transcript_identity)?;
    let transcript_len = transcript.metadata()?.len();
    if transcript_len != completion.transcript_len
        || completion
            .initial_len
            .checked_add(completion.bytes_appended)
            != Some(completion.transcript_len)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pipe sink completion verification failed",
        ));
    }
    Ok(completion)
}

pub fn remove_pipe_sink_ack_under_root(
    root: &Path,
    relative_path: impl AsRef<Path>,
) -> io::Result<()> {
    let relative_path = relative_path.as_ref();
    reject_uncontained_relative_path(relative_path)?;
    unlink_file_under_root(root, relative_path)
}

fn reject_uncontained_relative_path(path: &Path) -> io::Result<()> {
    if path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pipe sink path must be relative",
        ));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "pipe sink path must stay beneath asset root",
                ));
            }
        }
    }
    Ok(())
}

fn verify_identity(expected: &PipeSinkIdentity, actual: &PipeSinkIdentity) -> io::Result<()> {
    if expected.dev != actual.dev
        || expected.ino != actual.ino
        || expected.uid != actual.uid
        || expected.mode != actual.mode
        || actual.nlink != 1
        || expected.nlink != actual.nlink
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe sink identity check failed",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn identity_from_file(file: &std::fs::File) -> io::Result<PipeSinkIdentity> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat initializes stat for a valid open file descriptor.
    let result = unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat returned success, so stat is initialized.
    let stat = unsafe { stat.assume_init() };
    if (stat.st_mode & libc::S_IFMT) != libc::S_IFREG || stat.st_nlink != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pipe sink transcript verification failed",
        ));
    }
    Ok(PipeSinkIdentity {
        dev: stat.st_dev,
        ino: stat.st_ino,
        uid: stat.st_uid,
        mode: stat.st_mode & 0o777,
        nlink: stat.st_nlink,
    })
}

#[cfg(not(unix))]
fn identity_from_file(_: &std::fs::File) -> io::Result<PipeSinkIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable tmux transcript capture is not supported on this platform",
    ))
}

#[cfg(unix)]
fn open_existing_file_beneath(root: &Path, relative_path: &Path) -> io::Result<std::fs::File> {
    open_beneath(
        root,
        relative_path,
        libc::O_WRONLY | libc::O_APPEND | libc::O_NONBLOCK,
        0,
    )
}

#[cfg(unix)]
fn open_existing_transcript_for_verification(
    root: &Path,
    relative_path: &Path,
) -> io::Result<std::fs::File> {
    open_beneath(root, relative_path, libc::O_RDONLY | libc::O_NONBLOCK, 0)
}

#[cfg(not(unix))]
fn open_existing_file_beneath(root: &Path, relative_path: &Path) -> io::Result<std::fs::File> {
    OpenOptions::new()
        .append(true)
        .open(root.join(relative_path))
}

#[cfg(not(unix))]
fn open_existing_transcript_for_verification(
    root: &Path,
    relative_path: &Path,
) -> io::Result<std::fs::File> {
    OpenOptions::new().read(true).open(root.join(relative_path))
}

#[cfg(unix)]
fn write_ready_ack(
    root: &Path,
    relative_path: &Path,
    nonce: &str,
    transcript: &PipeSinkIdentity,
) -> io::Result<()> {
    write_ack_payload_under_root(
        root,
        relative_path,
        &PipeSinkAckPayload {
            nonce: nonce.to_string(),
            pid: process::id(),
            transcript_dev: transcript.dev,
            transcript_ino: transcript.ino,
        },
    )
}

#[cfg(not(unix))]
fn write_ready_ack(
    root: &Path,
    relative_path: &Path,
    nonce: &str,
    transcript: &PipeSinkIdentity,
) -> io::Result<()> {
    write_ack_payload_under_root(
        root,
        relative_path,
        &PipeSinkAckPayload {
            nonce: nonce.to_string(),
            pid: process::id(),
            transcript_dev: transcript.dev,
            transcript_ino: transcript.ino,
        },
    )
}

fn write_completion_ack(
    root: &Path,
    relative_path: &Path,
    payload: &PipeSinkCompletionPayload,
) -> io::Result<()> {
    write_ack_payload_under_root(root, relative_path, payload)
}

#[cfg(unix)]
fn write_ack_payload_under_root(
    root: &Path,
    relative_path: &Path,
    payload: &impl Serialize,
) -> io::Result<()> {
    let temp_relative_path = ack_temp_relative_path(relative_path)?;
    let write_result = (|| {
        let mut file = open_beneath(
            root,
            &temp_relative_path,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        serde_json::to_writer(&mut file, payload)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        sync_parent_directory_under_root(root, &temp_relative_path)?;
        rename_file_noreplace_under_root(root, &temp_relative_path, relative_path)
    })();
    if write_result.is_err() {
        let _ = unlink_file_under_root(root, &temp_relative_path);
    }
    write_result
}

#[cfg(not(unix))]
fn write_ack_payload_under_root(
    root: &Path,
    relative_path: &Path,
    payload: &impl Serialize,
) -> io::Result<()> {
    let path = root.join(relative_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp_path = path.with_file_name(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("pipe.ack"),
        process::id(),
        system_time_nanos()
    ));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        serde_json::to_writer(&mut file, payload)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        sync_parent_directory(&temp_path)?;
        std::fs::rename(&temp_path, &path)?;
        sync_parent_directory(&path)
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    write_result
}

#[cfg(unix)]
fn open_existing_ack_beneath(root: &Path, relative_path: &Path) -> io::Result<std::fs::File> {
    open_beneath(root, relative_path, libc::O_RDONLY | libc::O_NONBLOCK, 0)
}

#[cfg(not(unix))]
fn open_existing_ack_beneath(root: &Path, relative_path: &Path) -> io::Result<std::fs::File> {
    OpenOptions::new().read(true).open(root.join(relative_path))
}

#[cfg(unix)]
fn ensure_regular_file(file: &std::fs::File) -> io::Result<()> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat initializes stat for a valid open file descriptor.
    let result = unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat returned success, so stat is initialized.
    let stat = unsafe { stat.assume_init() };
    if (stat.st_mode & libc::S_IFMT) == libc::S_IFREG {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pipe sink acknowledgement verification failed",
        ))
    }
}

#[cfg(not(unix))]
fn ensure_regular_file(file: &std::fs::File) -> io::Result<()> {
    if file.metadata()?.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pipe sink acknowledgement verification failed",
        ))
    }
}

#[cfg(unix)]
fn ack_temp_relative_path(relative_path: &Path) -> io::Result<PathBuf> {
    let Some(file_name) = relative_path.file_name().and_then(|name| name.to_str()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ack path must include a file name",
        ));
    };
    let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
    Ok(parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        process::id(),
        system_time_nanos()
    )))
}

#[cfg(unix)]
fn rename_file_noreplace_under_root(
    root: &Path,
    source: &Path,
    destination: &Path,
) -> io::Result<()> {
    reject_uncontained_relative_path(source)?;
    reject_uncontained_relative_path(destination)?;
    let source_parent = source.parent().unwrap_or_else(|| Path::new(""));
    let destination_parent = destination.parent().unwrap_or_else(|| Path::new(""));
    if source_parent != destination_parent {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ack temporary file must share destination directory",
        ));
    }
    let Some(source_name) = source.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ack temporary path must include a file name",
        ));
    };
    let Some(destination_name) = destination.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ack path must include a file name",
        ));
    };
    let parent_file = open_dir_beneath(root, source_parent)?;
    let source_c = std::ffi::CString::new(source_name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "nul byte in path"))?;
    let destination_c = std::ffi::CString::new(destination_name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "nul byte in path"))?;
    #[cfg(target_os = "linux")]
    {
        // SAFETY: syscall receives a valid directory fd and two nul-terminated child names.
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                parent_file.as_raw_fd(),
                source_c.as_ptr(),
                parent_file.as_raw_fd(),
                destination_c.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: renameat receives a valid directory fd and two nul-terminated child names.
        let result = unsafe {
            libc::renameat(
                parent_file.as_raw_fd(),
                source_c.as_ptr(),
                parent_file.as_raw_fd(),
                destination_c.as_ptr(),
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    parent_file.sync_all()
}

#[cfg(unix)]
fn sync_parent_directory_under_root(root: &Path, relative_path: &Path) -> io::Result<()> {
    let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
    open_dir_beneath(root, parent)?.sync_all()
}

fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    #[cfg(unix)]
    {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY);
        options.open(parent)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
        Ok(())
    }
}

fn system_time_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(unix)]
fn unlink_file_under_root(root: &Path, relative_path: &Path) -> io::Result<()> {
    reject_uncontained_relative_path(relative_path)?;
    let Some(file_name) = relative_path.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ack path must include a file name",
        ));
    };
    let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
    let parent_file = open_dir_beneath(root, parent)?;
    let name = std::ffi::CString::new(file_name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "nul byte in path"))?;
    // SAFETY: unlinkat receives a valid directory fd and a nul-terminated child name.
    let result = unsafe { libc::unlinkat(parent_file.as_raw_fd(), name.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn unlink_file_under_root(root: &Path, relative_path: &Path) -> io::Result<()> {
    reject_uncontained_relative_path(relative_path)?;
    std::fs::remove_file(root.join(relative_path))
}

#[cfg(unix)]
fn open_beneath(
    root: &Path,
    relative_path: &Path,
    final_flags: libc::c_int,
    create_mode: libc::mode_t,
) -> io::Result<std::fs::File> {
    let mut root_options = OpenOptions::new();
    root_options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut current = root_options.open(root)?;
    let mut components = relative_path.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pipe sink path must use normal components",
            ));
        };
        let name = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "nul byte in path"))?;
        let is_last = components.peek().is_none();
        let flags = if is_last {
            final_flags | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };
        // SAFETY: openat receives a valid directory fd and a nul-terminated relative component.
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags, create_mode) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd was returned by openat and is owned by this File.
        current = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(current)
}

#[cfg(unix)]
fn open_dir_beneath(root: &Path, relative_path: &Path) -> io::Result<std::fs::File> {
    if relative_path.as_os_str().is_empty() {
        let mut root_options = OpenOptions::new();
        root_options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        return root_options.open(root);
    }
    open_beneath(root, relative_path, libc::O_RDONLY | libc::O_DIRECTORY, 0)
}

#[cfg(all(unix, target_os = "linux"))]
fn verify_helper_process(
    ack: &PipeSinkAckPayload,
    expected_transcript: &PipeSinkIdentity,
    expected_exe: &Path,
) -> io::Result<()> {
    if ack.pid <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe sink acknowledgement verification failed",
        ));
    }
    verify_helper_exe(ack.pid, expected_exe)?;
    verify_helper_open_transcript(ack.pid, expected_transcript)?;
    Ok(())
}

#[cfg(not(all(unix, target_os = "linux")))]
fn verify_helper_process(_: &PipeSinkAckPayload, _: &PipeSinkIdentity, _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable tmux transcript capture is not supported on this platform",
    ))
}

#[cfg(all(unix, target_os = "linux"))]
fn helper_process_start_time_ticks(pid: u32) -> io::Result<u64> {
    if pid <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe sink process identity verification failed",
        ));
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "pipe sink process identity verification failed",
            )
        } else {
            err
        }
    })?;
    let fields = stat
        .rsplit_once(')')
        .map(|(_, fields)| fields)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "pipe sink process identity verification failed",
            )
        })?;
    fields
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "pipe sink process identity verification failed",
            )
        })?
        .parse::<u64>()
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "pipe sink process identity verification failed",
            )
        })
}

#[cfg(not(all(unix, target_os = "linux")))]
fn helper_process_start_time_ticks(_: u32) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable tmux transcript capture is not supported on this platform",
    ))
}

#[cfg(all(unix, target_os = "linux"))]
fn verify_helper_exe(pid: u32, expected_exe: &Path) -> io::Result<()> {
    let actual =
        std::fs::read_link(format!("/proc/{pid}/exe")).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => io::Error::new(
                io::ErrorKind::PermissionDenied,
                "pipe sink acknowledgement verification failed",
            ),
            _ => err,
        })?;
    let expected = match std::fs::canonicalize(expected_exe) {
        Ok(expected) => expected,
        Err(_) => expected_exe.to_path_buf(),
    };
    if actual == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe sink acknowledgement verification failed",
        ))
    }
}

#[cfg(all(unix, target_os = "linux"))]
fn verify_helper_open_transcript(pid: u32, expected: &PipeSinkIdentity) -> io::Result<()> {
    let entries = std::fs::read_dir(format!("/proc/{pid}/fd")).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe sink acknowledgement verification failed",
        ),
        _ => err,
    })?;
    for entry in entries {
        let entry = entry?;
        let metadata = match std::fs::metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.dev() == expected.dev && metadata.ino() == expected.ino {
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "pipe sink acknowledgement verification failed",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor;

    #[cfg(target_os = "linux")]
    #[test]
    fn durable_pipe_capture_platform_check_accepts_linux() {
        ensure_durable_pipe_capture_supported().unwrap();
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn durable_pipe_capture_platform_check_rejects_unsupported_platforms() {
        let error = ensure_durable_pipe_capture_supported().unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn pipe_completion_is_not_published_when_transcript_sync_fails() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("temp")
            .join("pipe-sink-sync-failure");
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(root.join("activation")).unwrap();
        let transcript = root.join("activation/transcript.pipe.log");
        fs::write(&transcript, "").unwrap();
        let identity = pipe_sink_identity(&transcript).unwrap();
        let ready = PipeSinkAckRequest::new("activation/pipe.ready", "sync-failure");
        let completion = PipeSinkAckRequest::new("activation/pipe.complete", "sync-failure");

        let result = append_reader_to_pipe_log_under_root_with_sync(
            &root,
            Path::new("activation/transcript.pipe.log"),
            &identity,
            Some(&ready),
            Some(&completion),
            &mut Cursor::new(b"tail\n"),
            |_| Err(io::Error::other("sync failed")),
        );

        assert!(result.is_err());
        assert!(root.join("activation/pipe.ready").is_file());
        assert!(!root.join("activation/pipe.complete").exists());
    }
}
