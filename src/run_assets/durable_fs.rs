use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process;

use crate::pipe_sink::{PipeSinkIdentity, pipe_sink_identity_from_file};

use super::{RunAssetError, now_ms};

mod secure;

pub(crate) use secure::{
    EntryKind, SecureDir, SecureEntry, SecureFsError, open_dir_path, open_parent,
};

#[cfg(test)]
use std::os::unix::fs::DirBuilderExt;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum DirectorySyncEvent {
    #[cfg(test)]
    DirectoryCreated,
    #[cfg(test)]
    DirectoryEntryCreated,
    FileCreated,
    FileRenamed,
}

pub(super) fn create_dir_all(path: &Path) -> Result<(), RunAssetError> {
    create_dir_all_with_directory_sync(path, &mut sync_directory_event)
}

fn create_dir_all_with_directory_sync(
    path: &Path,
    _sync: &mut impl FnMut(&Path, DirectorySyncEvent) -> Result<(), RunAssetError>,
) -> Result<(), RunAssetError> {
    open_dir_path(path, true, false)
        .map(|_| ())
        .map_err(|err| private_directory_error("create", path, err))
}

pub(super) fn ensure_private_dir(path: &Path) -> Result<(), RunAssetError> {
    let directory = open_dir_path(path, false, false)
        .map_err(|err| private_directory_error("open", path, err))?;
    directory
        .ensure_private()
        .map_err(|err| private_directory_error("secure", path, err))
}

pub(super) fn validate_private_dir_path(path: &Path) -> Result<(), RunAssetError> {
    open_dir_path(path, false, true)
        .map(|_| ())
        .map_err(|err| private_directory_error("validate", path, err))
}

pub(crate) fn open_private_lock_file(path: &Path) -> Result<fs::File, RunAssetError> {
    let (parent, name) = open_parent(path, true)
        .map_err(|err| private_directory_error("open parent for", path, err))?;
    parent
        .validate_private()
        .map_err(|err| private_directory_error("validate parent for", path, err))?;
    parent
        .open_or_create_lock_file(&name)
        .map_err(|err| RunAssetError::new(format!("open private lock failed: {err}")))
}

pub(crate) fn read_private_directory(
    path: &Path,
) -> Result<Option<super::PrivateDirectoryFiles>, RunAssetError> {
    let directory = match open_dir_path(path, false, true) {
        Ok(directory) => directory,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(private_directory_error("open", path, err)),
    };
    let mut files = Vec::new();
    for entry in directory
        .entries()
        .map_err(|err| private_directory_error("enumerate", path, err))?
    {
        let bytes = directory.read_file(&entry.name).map_err(|err| {
            RunAssetError::new(format!("read private directory entry failed: {err}"))
        })?;
        files.push((entry.name, bytes));
    }
    Ok(Some(files))
}

pub(crate) fn remove_regular_private(path: &Path) -> Result<(), RunAssetError> {
    let (parent, name) = open_parent(path, false)
        .map_err(|err| private_directory_error("open parent for", path, err))?;
    parent
        .validate_private()
        .map_err(|err| private_directory_error("validate parent for", path, err))?;
    parent
        .remove_regular_file(&name)
        .map_err(|err| RunAssetError::new(format!("remove private file failed: {err}")))
}

fn private_directory_error(action: &str, path: &Path, err: SecureFsError) -> RunAssetError {
    RunAssetError::new(format!(
        "{action} private directory {} failed: {err}",
        path.display()
    ))
}

pub(super) fn open_pipe_log_private(path: &Path) -> Result<PipeSinkIdentity, RunAssetError> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
        ensure_private_dir(parent)?;
    }
    reject_existing_symlink(path)?;
    let mut create_options = OpenOptions::new();
    create_options.append(true).create_new(true);
    #[cfg(unix)]
    {
        create_options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let (file, created) = match create_options.open(path) {
        Ok(file) => (file, true),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut existing_options = OpenOptions::new();
            existing_options.append(true);
            #[cfg(unix)]
            {
                existing_options
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
            }
            let file = existing_options.open(path).map_err(|open_err| {
                RunAssetError::new(format!(
                    "open transcript pipe sink {} failed: {open_err}",
                    path.display()
                ))
            })?;
            (file, false)
        }
        Err(err) => {
            return Err(RunAssetError::new(format!(
                "open transcript pipe sink {} failed: {err}",
                path.display()
            )));
        }
    };
    if created {
        set_private_open_permissions(&file, path, 0o600)?;
    }
    validate_private_regular_open_file(&file, path, 0o600)?;
    let pipe_identity = pipe_sink_identity_from_file(&file).map_err(|err| {
        RunAssetError::new(format!(
            "inspect transcript pipe sink {} failed: {err}",
            path.display()
        ))
    })?;
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and does not dereference pointers.
        let effective_uid = unsafe { libc::geteuid() };
        if pipe_identity.uid != effective_uid || pipe_identity.mode != 0o600 {
            return Err(RunAssetError::new(format!(
                "transcript pipe sink {} has unexpected ownership or permissions",
                path.display()
            )));
        }
    }
    file.sync_all().map_err(|err| {
        RunAssetError::new(format!(
            "sync transcript pipe sink {} failed: {err}",
            path.display()
        ))
    })?;
    drop(file);
    if created {
        sync_parent_for(
            path,
            DirectorySyncEvent::FileCreated,
            &mut sync_directory_event,
        )?;
    }
    Ok(pipe_identity)
}

pub(crate) fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), RunAssetError> {
    atomic_write_private_with_directory_sync(path, bytes, &mut sync_directory_event)
}

fn atomic_write_private_with_directory_sync(
    path: &Path,
    bytes: &[u8],
    sync: &mut impl FnMut(&Path, DirectorySyncEvent) -> Result<(), RunAssetError>,
) -> Result<(), RunAssetError> {
    if let Some(parent) = path.parent() {
        create_dir_all_with_directory_sync(parent, sync)?;
        ensure_private_dir(parent)?;
    }
    validate_private_file_parent(path)?;
    reject_existing_symlink(path)?;
    reject_existing_non_regular(path)?;
    let temp_path = temp_sibling_path(path);
    reject_existing_symlink(&temp_path)?;
    let write_result =
        write_new_private_with_directory_sync(&temp_path, bytes, sync).and_then(|()| {
            fs::rename(&temp_path, path).map_err(|err| {
                RunAssetError::new(format!(
                    "replace asset file {} failed: {err}",
                    path.display()
                ))
            })?;
            sync_parent_for(path, DirectorySyncEvent::FileRenamed, sync)
        });
    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    write_result
}

pub(crate) fn read_regular_private(path: &Path) -> Result<Option<Vec<u8>>, RunAssetError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(RunAssetError::new(format!(
                "asset file {} is a symlink",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(RunAssetError::new(format!(
                "asset file {} is not a regular file",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(RunAssetError::new(format!(
                "inspect asset file {} failed: {err}",
                path.display()
            )));
        }
    }
    validate_private_file_parent(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|err| {
        RunAssetError::new(format!("open asset file {} failed: {err}", path.display()))
    })?;
    validate_private_regular_open_file(&file, path, 0o600)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|err| {
        RunAssetError::new(format!("read asset file {} failed: {err}", path.display()))
    })?;
    Ok(Some(bytes))
}

pub(crate) fn append_private_line(path: &Path, line: &[u8]) -> Result<(), RunAssetError> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
        ensure_private_dir(parent)?;
    }
    validate_private_file_parent(path)?;
    let created = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(RunAssetError::new(format!(
                "asset file {} is a symlink",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(RunAssetError::new(format!(
                "asset file {} is not a regular file",
                path.display()
            )));
        }
        Ok(_) => false,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
        Err(err) => {
            return Err(RunAssetError::new(format!(
                "inspect asset file {} failed: {err}",
                path.display()
            )));
        }
    };
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|err| {
        RunAssetError::new(format!("open asset file {} failed: {err}", path.display()))
    })?;
    if created {
        set_private_open_permissions(&file, path, 0o600)?;
    }
    validate_private_regular_open_file(&file, path, 0o600)?;
    file.write_all(line).map_err(|err| {
        RunAssetError::new(format!("write asset file {} failed: {err}", path.display()))
    })?;
    file.sync_all().map_err(|err| {
        RunAssetError::new(format!("sync asset file {} failed: {err}", path.display()))
    })?;
    drop(file);
    if created {
        sync_parent_for(
            path,
            DirectorySyncEvent::FileCreated,
            &mut sync_directory_event,
        )?;
    }
    Ok(())
}

pub(crate) fn truncate_private(path: &Path, len: u64) -> Result<(), RunAssetError> {
    validate_private_file_parent(path)?;
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|err| {
        RunAssetError::new(format!("open asset file {} failed: {err}", path.display()))
    })?;
    validate_private_regular_open_file(&file, path, 0o600)?;
    file.set_len(len).map_err(|err| {
        RunAssetError::new(format!(
            "truncate asset file {} failed: {err}",
            path.display()
        ))
    })?;
    file.sync_all().map_err(|err| {
        RunAssetError::new(format!("sync asset file {} failed: {err}", path.display()))
    })
}

pub(super) fn write_create_new_private(path: &Path, bytes: &[u8]) -> Result<(), RunAssetError> {
    let (parent, name) = open_parent(path, true)
        .map_err(|err| private_directory_error("open parent for", path, err))?;
    parent
        .validate_private()
        .map_err(|err| private_directory_error("validate parent for", path, err))?;
    parent
        .atomic_create_file(&name, bytes)
        .map_err(|err| RunAssetError::new(format!("create private file failed: {err}")))
}

fn write_new_private_with_directory_sync(
    path: &Path,
    bytes: &[u8],
    sync: &mut impl FnMut(&Path, DirectorySyncEvent) -> Result<(), RunAssetError>,
) -> Result<(), RunAssetError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|err| {
        RunAssetError::new(format!(
            "open new asset file {} failed: {err}",
            path.display()
        ))
    })?;
    file.write_all(bytes).map_err(|err| {
        RunAssetError::new(format!(
            "write new asset file {} failed: {err}",
            path.display()
        ))
    })?;
    set_private_open_permissions(&file, path, 0o600)?;
    validate_private_regular_open_file(&file, path, 0o600)?;
    file.sync_all().map_err(|err| {
        RunAssetError::new(format!(
            "sync new asset file {} failed: {err}",
            path.display()
        ))
    })?;
    drop(file);
    sync_parent_for(path, DirectorySyncEvent::FileCreated, sync)
}

#[cfg(test)]
fn create_private_dir_with_directory_sync(
    path: &Path,
    sync: &mut impl FnMut(&Path, DirectorySyncEvent) -> Result<(), RunAssetError>,
) -> Result<(), RunAssetError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path).map_err(|err| {
        RunAssetError::new(format!(
            "create run asset directory {} failed: {err}",
            path.display()
        ))
    })?;
    ensure_private_dir(path)?;
    sync(path, DirectorySyncEvent::DirectoryCreated)?;
    let parent = nonempty_parent(path);
    sync(parent, DirectorySyncEvent::DirectoryEntryCreated)?;
    Ok(())
}

fn sync_parent_for(
    path: &Path,
    event: DirectorySyncEvent,
    sync: &mut impl FnMut(&Path, DirectorySyncEvent) -> Result<(), RunAssetError>,
) -> Result<(), RunAssetError> {
    let parent = nonempty_parent(path);
    sync(parent, event)
}

fn nonempty_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sync_directory_event(path: &Path, event: DirectorySyncEvent) -> Result<(), RunAssetError> {
    sync_directory(path).map_err(|err| {
        RunAssetError::new(format!(
            "sync asset directory {} after {} failed: {err}",
            path.display(),
            directory_sync_event_name(event)
        ))
    })
}

fn directory_sync_event_name(event: DirectorySyncEvent) -> &'static str {
    match event {
        #[cfg(test)]
        DirectorySyncEvent::DirectoryCreated => "directory creation",
        #[cfg(test)]
        DirectorySyncEvent::DirectoryEntryCreated => "directory entry creation",
        DirectorySyncEvent::FileCreated => "file creation",
        DirectorySyncEvent::FileRenamed => "file rename",
    }
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY);
        options.open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn set_private_open_permissions(
    file: &fs::File,
    path: &Path,
    mode: u32,
) -> Result<(), RunAssetError> {
    #[cfg(unix)]
    {
        // SAFETY: fchmod operates on the valid descriptor owned by file.
        let result = unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) };
        if result != 0 {
            return Err(RunAssetError::new(format!(
                "set private permissions {} failed: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, path, mode);
    }
    Ok(())
}

fn validate_private_regular_open_file(
    file: &fs::File,
    path: &Path,
    mode: u32,
) -> Result<(), RunAssetError> {
    let open_metadata = file.metadata().map_err(|err| {
        RunAssetError::new(format!(
            "inspect asset file {} failed: {err}",
            path.display()
        ))
    })?;
    let path_metadata = fs::symlink_metadata(path).map_err(|err| {
        RunAssetError::new(format!(
            "inspect asset file {} failed: {err}",
            path.display()
        ))
    })?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.is_file()
        || !open_metadata.is_file()
    {
        return Err(RunAssetError::new(format!(
            "asset file {} is not a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        validate_private_regular_metadata_identity(&open_metadata, path, mode)?;
        validate_private_regular_metadata_identity(&path_metadata, path, mode)?;
        if open_metadata.dev() != path_metadata.dev() || open_metadata.ino() != path_metadata.ino()
        {
            return Err(RunAssetError::new(format!(
                "asset file {} changed identity while being opened",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_private_metadata_identity(
    metadata: &fs::Metadata,
    path: &Path,
    mode: u32,
) -> Result<(), RunAssetError> {
    #[cfg(unix)]
    {
        let effective_uid = unsafe { libc::geteuid() };
        if metadata.uid() != effective_uid || metadata.permissions().mode() & 0o777 != mode {
            return Err(RunAssetError::new(format!(
                "asset file {} must be current-user mode {mode:o}",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_regular_metadata_identity(
    metadata: &fs::Metadata,
    path: &Path,
    mode: u32,
) -> Result<(), RunAssetError> {
    validate_private_metadata_identity(metadata, path, mode)?;
    if metadata.nlink() != 1 {
        return Err(RunAssetError::new(format!(
            "asset file {} must have exactly one link",
            path.display()
        )));
    }
    Ok(())
}

fn validate_private_file_parent(path: &Path) -> Result<(), RunAssetError> {
    let parent = nonempty_parent(path);
    validate_private_dir_path(parent)
}

fn reject_existing_symlink(path: &Path) -> Result<(), RunAssetError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(RunAssetError::new(format!(
            "asset file {} is a symlink",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(RunAssetError::new(format!(
            "inspect asset file {} failed: {err}",
            path.display()
        ))),
    }
}

fn reject_existing_non_regular(path: &Path) -> Result<(), RunAssetError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            #[cfg(unix)]
            {
                validate_private_regular_metadata_identity(&metadata, path, 0o600)?;
            }
            Ok(())
        }
        Ok(_) => Err(RunAssetError::new(format!(
            "asset file {} is not a regular file",
            path.display()
        ))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(RunAssetError::new(format!(
            "inspect asset file {} failed: {err}",
            path.display()
        ))),
    }
}

#[cfg(not(unix))]
fn reject_symlink(path: &Path) -> Result<(), RunAssetError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(RunAssetError::new(format!(
            "asset path {} is a symlink",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(err) => Err(RunAssetError::new(format!(
            "inspect asset path {} failed: {err}",
            path.display()
        ))),
    }
}

fn temp_sibling_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("asset");
    path.with_file_name(format!(".{name}.tmp-{}-{}", process::id(), now_ms()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        DirectorySyncEvent, atomic_write_private_with_directory_sync, create_dir_all,
        create_private_dir_with_directory_sync, read_regular_private, sync_directory,
        sync_parent_for,
    };
    use crate::run_assets::RunAssetError;

    fn test_temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "humanize-run-assets-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn atomic_replace_syncs_parent_after_file_create_and_rename_in_order() {
        let root = test_temp_dir("atomic-sync-order");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("manifest.json");
        let mut events = Vec::new();

        atomic_write_private_with_directory_sync(&path, b"payload", &mut |directory, event| {
            events.push((directory.to_path_buf(), event));
            Ok(())
        })
        .unwrap();

        assert_eq!(
            events,
            vec![
                (root.clone(), DirectorySyncEvent::FileCreated),
                (root.clone(), DirectorySyncEvent::FileRenamed),
            ]
        );
        assert_eq!(fs::read(&path).unwrap(), b"payload");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_replace_surfaces_parent_sync_failure_after_rename() {
        let root = test_temp_dir("atomic-sync-failure");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("manifest.json");

        let error = atomic_write_private_with_directory_sync(
            &path,
            b"payload",
            &mut |_directory, event| {
                if event == DirectorySyncEvent::FileRenamed {
                    Err(RunAssetError::new("injected directory sync failure"))
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("rename parent sync failure must be returned");

        assert!(
            error
                .to_string()
                .contains("injected directory sync failure")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_replace_removes_temporary_file_when_file_create_sync_fails() {
        let root = test_temp_dir("atomic-create-sync-failure");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("manifest.json");

        let error = atomic_write_private_with_directory_sync(
            &path,
            b"payload",
            &mut |_directory, event| {
                if event == DirectorySyncEvent::FileCreated {
                    Err(RunAssetError::new("injected file create sync failure"))
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("file creation parent sync failure must be returned");

        assert!(
            error
                .to_string()
                .contains("injected file create sync failure")
        );
        assert!(!path.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_create_syncs_new_directory_before_parent_entry() {
        let root = test_temp_dir("directory-sync-order");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("run");
        let mut events = Vec::new();

        create_private_dir_with_directory_sync(&path, &mut |directory, event| {
            events.push((directory.to_path_buf(), event));
            Ok(())
        })
        .unwrap();

        assert_eq!(
            events,
            vec![
                (path.clone(), DirectorySyncEvent::DirectoryCreated),
                (root.clone(), DirectorySyncEvent::DirectoryEntryCreated),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_private_directory_creation_reopens_and_validates_the_winner() {
        let root = test_temp_dir("concurrent-directory-create");
        fs::create_dir_all(&root).unwrap();
        let path = Arc::new(root.join("shared/deep"));
        let barrier = Arc::new(Barrier::new(16));
        let mut threads = Vec::new();

        for _ in 0..16 {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            threads.push(thread::spawn(move || {
                barrier.wait();
                create_dir_all(&path)
            }));
        }

        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relative_file_sync_uses_current_directory_as_parent() {
        let mut observed = None;

        sync_parent_for(
            std::path::Path::new("manifest.json"),
            DirectorySyncEvent::FileCreated,
            &mut |directory, _event| {
                observed = Some(directory.to_path_buf());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(observed, Some(PathBuf::from(".")));
    }

    #[cfg(unix)]
    #[test]
    fn missing_optional_private_file_under_absent_directory_reads_as_none() {
        use std::os::unix::fs::PermissionsExt;

        let root = test_temp_dir("missing-optional-file");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("driver").join("driver-events.jsonl");

        assert_eq!(read_regular_private(&path).unwrap(), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_directory_sync_accepts_a_real_directory_descriptor() {
        let root = test_temp_dir("linux-directory-sync");
        fs::create_dir_all(&root).unwrap();

        sync_directory(&root).unwrap();

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_file_identity_rejects_foreign_owner_metadata() {
        let metadata = fs::metadata("/etc/passwd").unwrap();
        let error = super::validate_private_metadata_identity(
            &metadata,
            std::path::Path::new("/etc/passwd"),
            0o600,
        )
        .expect_err("foreign or public authority file must be rejected");

        assert!(error.to_string().contains("current-user mode 600"));
    }
}
