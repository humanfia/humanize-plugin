use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::run_assets::durable_fs::{
    EntryKind, SecureDir, SecureEntry, SecureFsError, open_dir_path, open_parent,
};

use super::FlowLock;

const FLOW_MANIFEST: &str = "flow.json";
static NEXT_STAGING: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct FlowLockDirectoryError {
    message: String,
}

impl FlowLockDirectoryError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FlowLockDirectoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FlowLockDirectoryError {}

impl From<io::Error> for FlowLockDirectoryError {
    fn from(error: io::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<serde_json::Error> for FlowLockDirectoryError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<SecureFsError> for FlowLockDirectoryError {
    fn from(error: SecureFsError) -> Self {
        Self::new(error.to_string())
    }
}

pub(super) fn write_directory(lock: &FlowLock, root: &Path) -> Result<(), FlowLockDirectoryError> {
    let (parent, final_name) = open_parent(root, true)?;
    if parent.entry(&final_name)?.is_some() {
        return Err(FlowLockDirectoryError::new(
            "flow lock directory already exists",
        ));
    }
    cleanup_staging_directories(&parent, &final_name)?;

    let staging_name = staging_name(&final_name);
    let staging = parent.create_child_dir(&staging_name)?;
    let result = (|| {
        write_directory_contents(lock, &staging)?;
        staging.sync()?;
        drop(staging);
        parent.rename_child_noreplace(&staging_name, &final_name)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = parent.remove_child_tree(&staging_name);
    }
    result
}

fn write_directory_contents(
    lock: &FlowLock,
    root: &SecureDir,
) -> Result<(), FlowLockDirectoryError> {
    let mut manifest = serde_json::to_value(lock)?;
    for resource in manifest_resources_mut(&mut manifest)? {
        resource
            .as_object_mut()
            .ok_or_else(|| FlowLockDirectoryError::new("flow resource must be an object"))?
            .remove("content");
    }
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    manifest_bytes.push(b'\n');
    root.create_file(OsStr::new(FLOW_MANIFEST), &manifest_bytes)?;

    for resource in &lock.draft().resources {
        let relative = checked_relative_path(&resource.id)?;
        write_relative_file(root, relative, resource.source.as_bytes())?;
    }
    Ok(())
}

pub(super) fn load_directory(root: &Path) -> Result<FlowLock, FlowLockDirectoryError> {
    let root = open_dir_path(root, false, true)?;
    let manifest_bytes = root.read_file(OsStr::new(FLOW_MANIFEST))?;
    let mut manifest = serde_json::from_slice::<Value>(&manifest_bytes)?;
    let mut allowed_files = BTreeSet::from([PathBuf::from(FLOW_MANIFEST)]);
    for resource in manifest_resources_mut(&mut manifest)? {
        let object = resource
            .as_object_mut()
            .ok_or_else(|| FlowLockDirectoryError::new("flow resource must be an object"))?;
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| FlowLockDirectoryError::new("flow resource path is required"))?;
        let relative = checked_relative_path(path)?.to_path_buf();
        if !allowed_files.insert(relative.clone()) {
            return Err(FlowLockDirectoryError::new(
                "flow resource path is duplicated",
            ));
        }
        let content = read_relative_file(&root, &relative)?;
        let content = String::from_utf8(content)
            .map_err(|_| FlowLockDirectoryError::new("flow resource is not valid UTF-8"))?;
        object.insert("content".into(), Value::String(content));
    }

    let allowed_directories = allowed_directories(&allowed_files);
    validate_tree(&root, Path::new(""), &allowed_files, &allowed_directories)?;
    Ok(serde_json::from_value(manifest)?)
}

fn manifest_resources_mut(value: &mut Value) -> Result<&mut Vec<Value>, FlowLockDirectoryError> {
    value
        .get_mut("flow")
        .and_then(Value::as_object_mut)
        .and_then(|flow| flow.get_mut("resources"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| FlowLockDirectoryError::new("flow lock resources are missing"))
}

fn checked_relative_path(value: &str) -> Result<&Path, FlowLockDirectoryError> {
    let path = Path::new(value);
    if value.is_empty()
        || value == FLOW_MANIFEST
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(FlowLockDirectoryError::new(
            "flow resource path must stay inside the package",
        ));
    }
    Ok(path)
}

fn write_relative_file(
    root: &SecureDir,
    relative: &Path,
    bytes: &[u8],
) -> Result<(), FlowLockDirectoryError> {
    let (directory, name) = relative_parent(root, relative, true)?;
    directory.create_file(&name, bytes)?;
    Ok(())
}

fn read_relative_file(
    root: &SecureDir,
    relative: &Path,
) -> Result<Vec<u8>, FlowLockDirectoryError> {
    let (directory, name) = relative_parent(root, relative, false)?;
    Ok(directory.read_file(&name)?)
}

fn relative_parent(
    root: &SecureDir,
    relative: &Path,
    create: bool,
) -> Result<(SecureDir, OsString), FlowLockDirectoryError> {
    let name = relative
        .file_name()
        .ok_or_else(|| FlowLockDirectoryError::new("package file name is missing"))?
        .to_os_string();
    let mut directory = root.try_clone()?;
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            let Component::Normal(name) = component else {
                return Err(FlowLockDirectoryError::new("invalid package path"));
            };
            directory = match directory.open_child_dir(name, true) {
                Ok(child) => child,
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    directory.create_child_dir(name)?
                }
                Err(error) => return Err(error.into()),
            };
        }
    }
    Ok((directory, name))
}

fn validate_tree(
    directory: &SecureDir,
    prefix: &Path,
    allowed_files: &BTreeSet<PathBuf>,
    allowed_directories: &BTreeSet<PathBuf>,
) -> Result<(), FlowLockDirectoryError> {
    for entry in directory.entries()? {
        let relative = prefix.join(&entry.name);
        match entry.kind {
            EntryKind::Directory => {
                validate_private_directory_entry(&entry)?;
                if !allowed_directories.contains(&relative) {
                    return Err(FlowLockDirectoryError::new(
                        "flow lock directory contains an undeclared directory",
                    ));
                }
                let child = directory.open_child_dir(&entry.name, true)?;
                validate_tree(&child, &relative, allowed_files, allowed_directories)?;
            }
            EntryKind::Regular => {
                validate_private_file_entry(&entry)?;
                if !allowed_files.contains(&relative) {
                    return Err(FlowLockDirectoryError::new(
                        "flow lock directory contains an undeclared file",
                    ));
                }
            }
            EntryKind::Symlink | EntryKind::Other => {
                return Err(FlowLockDirectoryError::new(
                    "flow lock directory contains an unsafe entry",
                ));
            }
        }
    }
    Ok(())
}

fn validate_private_directory_entry(entry: &SecureEntry) -> Result<(), FlowLockDirectoryError> {
    if entry.uid != effective_uid() || entry.mode != 0o700 || entry.nlink < 2 {
        return Err(FlowLockDirectoryError::new(
            "flow lock directories must be current-user mode 0700",
        ));
    }
    Ok(())
}

fn validate_private_file_entry(entry: &SecureEntry) -> Result<(), FlowLockDirectoryError> {
    if entry.uid != effective_uid() || entry.mode != 0o600 || entry.nlink != 1 {
        return Err(FlowLockDirectoryError::new(
            "flow lock files must be current-user mode 0600 single-link files",
        ));
    }
    Ok(())
}

fn allowed_directories(files: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    let mut directories = BTreeSet::new();
    for file in files {
        let mut parent = file.parent();
        while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
            directories.insert(path.to_path_buf());
            parent = path.parent();
        }
    }
    directories
}

fn cleanup_staging_directories(
    parent: &SecureDir,
    final_name: &OsStr,
) -> Result<(), FlowLockDirectoryError> {
    let prefix = staging_prefix(final_name);
    for entry in parent.entries()? {
        if !entry.name.as_bytes().starts_with(&prefix) {
            continue;
        }
        if entry.kind != EntryKind::Directory || entry.uid != effective_uid() || entry.mode != 0o700
        {
            return Err(FlowLockDirectoryError::new(
                "unsafe flow lock staging entry",
            ));
        }
        parent.remove_child_tree(&entry.name)?;
    }
    Ok(())
}

fn staging_prefix(final_name: &OsStr) -> Vec<u8> {
    let mut prefix = b".".to_vec();
    prefix.extend_from_slice(final_name.as_bytes());
    prefix.extend_from_slice(b".staging-");
    prefix
}

fn staging_name(final_name: &OsStr) -> OsString {
    let mut bytes = staging_prefix(final_name);
    bytes.extend_from_slice(
        format!(
            "{}-{}",
            std::process::id(),
            NEXT_STAGING.fetch_add(1, Ordering::Relaxed)
        )
        .as_bytes(),
    );
    OsString::from_vec(bytes)
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}
