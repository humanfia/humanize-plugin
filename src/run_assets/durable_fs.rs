use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;

use crate::pipe_sink::{PipeSinkIdentity, pipe_sink_identity_from_file};

use super::{RunAssetError, now_ms};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum DirectorySyncEvent {
    DirectoryCreated,
    DirectoryEntryCreated,
    FileCreated,
    FileRenamed,
}

pub(super) fn create_dir_all(path: &Path) -> Result<(), RunAssetError> {
    create_dir_all_with_directory_sync(path, &mut sync_directory_event)
}

fn create_dir_all_with_directory_sync(
    path: &Path,
    sync: &mut impl FnMut(&Path, DirectorySyncEvent) -> Result<(), RunAssetError>,
) -> Result<(), RunAssetError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if current.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(RunAssetError::new(format!(
                    "asset path component {} is a symlink",
                    current.display()
                )));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(RunAssetError::new(format!(
                    "asset path component {} is not a directory",
                    current.display()
                )));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                create_private_dir_with_directory_sync(&current, sync)?;
            }
            Err(err) => {
                return Err(RunAssetError::new(format!(
                    "inspect run asset directory {} failed: {err}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn ensure_private_dir(path: &Path) -> Result<(), RunAssetError> {
    #[cfg(unix)]
    {
        chmod_no_follow(path, 0o700, true)?;
    }
    #[cfg(not(unix))]
    {
        reject_symlink(path)?;
    }
    Ok(())
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
        ensure_private_open_file(&file, path, 0o600)?;
    }
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

pub(super) fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), RunAssetError> {
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
    reject_existing_symlink(path)?;
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

pub(super) fn write_create_new_private(path: &Path, bytes: &[u8]) -> Result<(), RunAssetError> {
    reject_existing_symlink(path)?;
    write_new_private_with_directory_sync(path, bytes, &mut sync_directory_event)
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
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
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
    ensure_private_open_file(&file, path, 0o600)?;
    file.sync_all().map_err(|err| {
        RunAssetError::new(format!(
            "sync new asset file {} failed: {err}",
            path.display()
        ))
    })?;
    drop(file);
    sync_parent_for(path, DirectorySyncEvent::FileCreated, sync)
}

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
        DirectorySyncEvent::DirectoryCreated => "directory creation",
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

fn ensure_private_open_file(file: &fs::File, path: &Path, mode: u32) -> Result<(), RunAssetError> {
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

#[cfg(unix)]
fn chmod_no_follow(path: &Path, mode: u32, directory: bool) -> Result<(), RunAssetError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    if directory {
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY);
    }
    let file = options.open(path).map_err(|err| {
        RunAssetError::new(format!(
            "open private asset path {} failed: {err}",
            path.display()
        ))
    })?;
    ensure_private_open_file(&file, path, mode)
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        DirectorySyncEvent, atomic_write_private_with_directory_sync,
        create_private_dir_with_directory_sync, sync_directory, sync_parent_for,
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_directory_sync_accepts_a_real_directory_descriptor() {
        let root = test_temp_dir("linux-directory-sync");
        fs::create_dir_all(&root).unwrap();

        sync_directory(&root).unwrap();

        fs::remove_dir_all(root).unwrap();
    }
}
