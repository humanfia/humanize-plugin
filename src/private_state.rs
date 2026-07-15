use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::run_assets::{atomic_write_private, read_regular_private};

const RUN_IDENTITY_FILE: &str = "identity.json";
const RUN_IDENTITY_SCHEMA: &str = "humanize.private_run_identity.v1";

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PrivateRunIdentity {
    schema: String,
    pub(crate) run_id: String,
    pub(crate) public_run_root: PathBuf,
    pub(crate) runs_root: PathBuf,
}

pub(crate) fn ensure_run_identity(
    runtime_root: &Path,
    public_run_root: &Path,
    runs_root: &Path,
    run_id: &str,
) -> io::Result<PrivateRunIdentity> {
    let private_run_root = ensure_private_run_root(runtime_root, public_run_root)?;
    let expected = PrivateRunIdentity {
        schema: RUN_IDENTITY_SCHEMA.to_string(),
        run_id: run_id.to_string(),
        public_run_root: absolute_path(public_run_root)?,
        runs_root: absolute_path(runs_root)?,
    };
    let path = private_run_root.join(RUN_IDENTITY_FILE);
    if let Some(existing) = read_identity_path(&path)? {
        if existing != expected {
            return Err(invalid_data(
                "private run identity conflicts with the requested run",
            ));
        }
        return Ok(existing);
    }
    let mut bytes = serde_json::to_vec_pretty(&expected).map_err(io::Error::other)?;
    bytes.push(b'\n');
    atomic_write_private(&path, &bytes).map_err(|err| io::Error::other(err.to_string()))?;
    read_identity_path(&path)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "private run identity disappeared after creation",
        )
    })
}

pub(crate) fn ensure_private_directory(path: &Path) -> io::Result<()> {
    crate::run_assets::ensure_private_directory(path)
        .map_err(|err| io::Error::other(err.to_string()))
}

pub(crate) fn ensure_private_run_root(
    runtime_root: &Path,
    public_run_root: &Path,
) -> io::Result<PathBuf> {
    ensure_private_directory(runtime_root)?;
    let private_run_root = crate::state_path::private_run_root(runtime_root, public_run_root);
    ensure_private_directory(&private_run_root)?;
    Ok(private_run_root)
}

pub(crate) fn read_run_identity(
    runtime_root: &Path,
    public_run_root: &Path,
) -> io::Result<Option<PrivateRunIdentity>> {
    read_identity_path(&identity_path(runtime_root, public_run_root))
}

pub(crate) fn discover_run_identities(runtime_root: &Path) -> io::Result<Vec<PrivateRunIdentity>> {
    let metadata = match fs::symlink_metadata(runtime_root) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    validate_private_directory(&metadata, "private runtime root")?;
    let mut identities = Vec::new();
    for entry in fs::read_dir(runtime_root)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        validate_private_directory(&metadata, "private run directory")?;
        let path = entry.path().join(RUN_IDENTITY_FILE);
        let identity = read_identity_path(&path)?.ok_or_else(|| {
            invalid_data(format!(
                "private run directory {} has no identity",
                entry.path().display()
            ))
        })?;
        let expected_root =
            crate::state_path::private_run_root(runtime_root, &identity.public_run_root);
        if expected_root != entry.path() {
            return Err(invalid_data("private run identity directory mismatch"));
        }
        identities.push(identity);
    }
    identities.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    Ok(identities)
}

pub(crate) fn discover_run_identities_for_runs_root(
    runtime_root: &Path,
    runs_root: &Path,
) -> io::Result<Vec<PrivateRunIdentity>> {
    let runs_root = absolute_path(runs_root)?;
    Ok(discover_run_identities(runtime_root)?
        .into_iter()
        .filter(|identity| identity.runs_root == runs_root)
        .collect())
}

pub(crate) fn identity_path(runtime_root: &Path, public_run_root: &Path) -> PathBuf {
    crate::state_path::private_run_root(runtime_root, public_run_root).join(RUN_IDENTITY_FILE)
}

fn read_identity_path(path: &Path) -> io::Result<Option<PrivateRunIdentity>> {
    let Some(bytes) =
        read_regular_private(path).map_err(|err| io::Error::other(err.to_string()))?
    else {
        return Ok(None);
    };
    let identity = serde_json::from_slice::<PrivateRunIdentity>(&bytes)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    if identity.schema != RUN_IDENTITY_SCHEMA || identity.run_id.is_empty() {
        return Err(invalid_data("private run identity schema is invalid"));
    }
    if !identity.public_run_root.is_absolute() || !identity.runs_root.is_absolute() {
        return Err(invalid_data("private run identity paths must be absolute"));
    }
    if identity.public_run_root.parent() != Some(identity.runs_root.as_path()) {
        return Err(invalid_data(
            "private run identity public root is outside its runs root",
        ));
    }
    Ok(Some(identity))
}

fn validate_private_directory(metadata: &fs::Metadata, label: &str) -> io::Result<()> {
    if !metadata.file_type().is_dir() {
        return Err(invalid_data(format!("{label} is not a real directory")));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(invalid_data(format!(
            "{label} owner is not the current user"
        )));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(invalid_data(format!(
            "{label} permissions must be 700, found {mode:o}"
        )));
    }
    Ok(())
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    std::path::absolute(path)
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
