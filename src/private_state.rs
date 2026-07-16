use std::collections::BTreeMap;
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
    let expected = expected_identity(public_run_root, runs_root, run_id)?;
    let private_run_root = ensure_private_run_root(runtime_root, public_run_root)?;
    let path = private_run_root.join(RUN_IDENTITY_FILE);
    if let Some(existing) = read_identity_path(&path)? {
        if !same_identity(&existing, &expected)? {
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
    let _ = absolute_path(public_run_root)?;
    ensure_private_directory(runtime_root)?;
    let private_run_root = private_run_root(runtime_root, public_run_root)?;
    ensure_private_directory(&private_run_root)?;
    Ok(private_run_root)
}

pub(crate) fn private_run_root(runtime_root: &Path, public_run_root: &Path) -> io::Result<PathBuf> {
    let public_run_root = absolute_path(public_run_root)?;
    let mut matches = stored_run_identities(runtime_root, false)?
        .into_iter()
        .filter(|stored| stored.public_run_root == public_run_root);
    let Some(stored) = matches.next() else {
        return crate::state_path::private_run_root(runtime_root, &public_run_root);
    };
    if matches.next().is_some() {
        return Err(invalid_data(
            "multiple private run identities resolve to the requested public run root",
        ));
    }
    Ok(stored.root)
}

pub(crate) fn private_run_root_for_socket_path(
    runtime_root: &Path,
    public_run_root: &Path,
) -> io::Result<PathBuf> {
    let canonical_root = crate::state_path::private_run_root(runtime_root, public_run_root)?;
    let metadata = match fs::symlink_metadata(runtime_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(canonical_root),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        validate_private_directory(&metadata, "private runtime root")?;
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        return Ok(canonical_root);
    }
    private_run_root(runtime_root, public_run_root)
}

pub(crate) fn read_run_identity(
    runtime_root: &Path,
    public_run_root: &Path,
) -> io::Result<Option<PrivateRunIdentity>> {
    read_identity_path(&identity_path(runtime_root, public_run_root)?)
}

pub(crate) fn discover_run_identities_for_runs_root(
    runtime_root: &Path,
    runs_root: &Path,
) -> io::Result<Vec<PrivateRunIdentity>> {
    let runs_root = absolute_path(runs_root)?;
    let stored = stored_run_identities(runtime_root, true)?;
    reject_conflicting_identities(&stored)?;
    let mut identities = stored
        .into_iter()
        .filter(|stored| stored.runs_root == runs_root)
        .map(|stored| stored.identity)
        .collect::<Vec<_>>();
    identities.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    Ok(identities)
}

pub(crate) fn identity_path(runtime_root: &Path, public_run_root: &Path) -> io::Result<PathBuf> {
    Ok(private_run_root(runtime_root, public_run_root)?.join(RUN_IDENTITY_FILE))
}

#[derive(Debug)]
struct StoredRunIdentity {
    root: PathBuf,
    identity: PrivateRunIdentity,
    public_run_root: PathBuf,
    runs_root: PathBuf,
}

fn stored_run_identities(
    runtime_root: &Path,
    require_identity: bool,
) -> io::Result<Vec<StoredRunIdentity>> {
    let metadata = match fs::symlink_metadata(runtime_root) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    validate_private_directory(&metadata, "private runtime root")?;
    let mut identities = Vec::new();
    for entry in fs::read_dir(runtime_root)? {
        let entry = entry?;
        let root = entry.path();
        let metadata = fs::symlink_metadata(&root)?;
        if !metadata.file_type().is_dir() {
            validate_private_directory(&metadata, "private run directory")?;
        }
        let identity_path = root.join(RUN_IDENTITY_FILE);
        let has_identity = match fs::symlink_metadata(&identity_path) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        if !has_identity && !require_identity {
            continue;
        }
        validate_private_directory(&metadata, "private run directory")?;
        let Some(identity) = read_identity_path(&identity_path)? else {
            return Err(invalid_data(format!(
                "private run directory {} has no identity",
                root.display()
            )));
        };
        let (public_run_root, runs_root) = resolved_identity_paths(&identity)?;
        let canonical_root = crate::state_path::private_run_root(runtime_root, &public_run_root)?;
        let legacy_root =
            crate::state_path::legacy_private_run_root(runtime_root, &identity.public_run_root);
        if root != canonical_root && root != legacy_root {
            return Err(invalid_data("private run identity directory mismatch"));
        }
        identities.push(StoredRunIdentity {
            root,
            identity,
            public_run_root,
            runs_root,
        });
    }
    Ok(identities)
}

fn expected_identity(
    public_run_root: &Path,
    runs_root: &Path,
    run_id: &str,
) -> io::Result<PrivateRunIdentity> {
    let public_run_root = absolute_path(public_run_root)?;
    let runs_root = absolute_path(runs_root)?;
    if public_run_root.parent() != Some(runs_root.as_path()) {
        return Err(invalid_data(
            "private run identity public root is outside its runs root",
        ));
    }
    Ok(PrivateRunIdentity {
        schema: RUN_IDENTITY_SCHEMA.to_string(),
        run_id: run_id.to_string(),
        public_run_root,
        runs_root,
    })
}

fn same_identity(left: &PrivateRunIdentity, right: &PrivateRunIdentity) -> io::Result<bool> {
    if left.schema != right.schema || left.run_id != right.run_id {
        return Ok(false);
    }
    Ok(resolved_identity_paths(left)? == resolved_identity_paths(right)?)
}

fn resolved_identity_paths(identity: &PrivateRunIdentity) -> io::Result<(PathBuf, PathBuf)> {
    let public_run_root = absolute_path(&identity.public_run_root)?;
    let runs_root = absolute_path(&identity.runs_root)?;
    if public_run_root.parent() != Some(runs_root.as_path()) {
        return Err(invalid_data(
            "private run identity public root is outside its runs root",
        ));
    }
    Ok((public_run_root, runs_root))
}

fn reject_conflicting_identities(identities: &[StoredRunIdentity]) -> io::Result<()> {
    let mut roots = BTreeMap::<PathBuf, &Path>::new();
    for identity in identities {
        if roots
            .insert(identity.public_run_root.clone(), identity.root.as_path())
            .is_some()
        {
            return Err(invalid_data(
                "multiple private run identities resolve to the same public run root",
            ));
        }
    }
    Ok(())
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
    crate::state_path::resolved_path(path)
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        PrivateRunIdentity, RUN_IDENTITY_SCHEMA, discover_run_identities_for_runs_root,
        ensure_private_run_root, ensure_run_identity, private_run_root, read_run_identity,
    };

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn symlinked_public_run_alias_shares_private_identity_and_discovery_scope() {
        let root = test_root("public-run-alias");
        let runtime_root = root.join("runtime");
        let real_runs_root = root.join("real-runs");
        let linked_runs_root = root.join("linked-runs");
        let real_run_root = real_runs_root.join("run-a");
        let linked_run_root = linked_runs_root.join("run-a");
        fs::create_dir_all(&runtime_root).unwrap();
        fs::create_dir_all(&real_run_root).unwrap();
        set_mode(&runtime_root, 0o700);
        symlink(&real_runs_root, &linked_runs_root).unwrap();

        let identity =
            ensure_run_identity(&runtime_root, &linked_run_root, &linked_runs_root, "run-a")
                .unwrap();

        assert_eq!(
            read_run_identity(&runtime_root, &real_run_root).unwrap(),
            Some(identity.clone())
        );
        assert_eq!(
            discover_run_identities_for_runs_root(&runtime_root, &real_runs_root).unwrap(),
            vec![identity]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_lexical_identity_is_reused_through_physical_public_run_alias() {
        let root = test_root("legacy-public-run-alias");
        let runtime_root = root.join("runtime");
        let real_runs_root = root.join("real-runs");
        let linked_runs_root = root.join("linked-runs");
        let real_run_root = real_runs_root.join("run-a");
        let linked_run_root = linked_runs_root.join("run-a");
        fs::create_dir_all(&runtime_root).unwrap();
        fs::create_dir_all(&real_run_root).unwrap();
        set_mode(&runtime_root, 0o700);
        symlink(&real_runs_root, &linked_runs_root).unwrap();

        let legacy_record = legacy_identity(&linked_run_root, &linked_runs_root, "run-a");
        let legacy_root = legacy_private_run_root(&runtime_root, &linked_run_root);
        write_identity(&legacy_root, &legacy_record);

        assert_eq!(
            read_run_identity(&runtime_root, &real_run_root).unwrap(),
            Some(legacy_record.clone())
        );
        assert_eq!(
            ensure_private_run_root(&runtime_root, &real_run_root).unwrap(),
            legacy_root
        );
        assert_eq!(
            ensure_private_run_root(&runtime_root, &linked_run_root).unwrap(),
            legacy_root
        );
        assert!(
            !crate::state_path::private_run_root(&runtime_root, &real_run_root)
                .unwrap()
                .exists()
        );
        assert_eq!(
            discover_run_identities_for_runs_root(&runtime_root, &real_runs_root).unwrap(),
            vec![legacy_record]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn conflicting_legacy_and_canonical_identities_fail_closed() {
        let root = test_root("conflicting-public-run-identities");
        let runtime_root = root.join("runtime");
        let real_runs_root = root.join("real-runs");
        let linked_runs_root = root.join("linked-runs");
        let real_run_root = real_runs_root.join("run-a");
        let linked_run_root = linked_runs_root.join("run-a");
        fs::create_dir_all(&runtime_root).unwrap();
        fs::create_dir_all(&real_run_root).unwrap();
        set_mode(&runtime_root, 0o700);
        symlink(&real_runs_root, &linked_runs_root).unwrap();

        let legacy_record = legacy_identity(&linked_run_root, &linked_runs_root, "run-a");
        let legacy_root = legacy_private_run_root(&runtime_root, &linked_run_root);
        write_identity(&legacy_root, &legacy_record);

        let canonical_identity = legacy_identity(&real_run_root, &real_runs_root, "run-a");
        let canonical_root =
            crate::state_path::private_run_root(&runtime_root, &real_run_root).unwrap();
        write_identity(&canonical_root, &canonical_identity);

        assert!(ensure_private_run_root(&runtime_root, &real_run_root).is_err());
        assert!(read_run_identity(&runtime_root, &real_run_root).is_err());
        assert!(discover_run_identities_for_runs_root(&runtime_root, &real_runs_root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unrelated_nonprivate_sibling_does_not_block_private_run_resolution() {
        let root = test_root("unrelated-runtime-sibling");
        let runtime_root = root.join("runtime");
        let runs_root = root.join("runs");
        let run_root = runs_root.join("run-a");
        fs::create_dir_all(&runtime_root).unwrap();
        fs::create_dir_all(&run_root).unwrap();
        set_mode(&runtime_root, 0o700);

        let identity = ensure_run_identity(&runtime_root, &run_root, &runs_root, "run-a").unwrap();
        let target_root = private_run_root(&runtime_root, &run_root).unwrap();
        let unrelated = runtime_root.join("scratch");
        fs::create_dir(&unrelated).unwrap();
        set_mode(&unrelated, 0o755);

        assert_eq!(
            private_run_root(&runtime_root, &run_root).unwrap(),
            target_root
        );
        assert_eq!(
            crate::driver::socket_path_for_run_root(&runtime_root, &run_root).unwrap(),
            target_root.join("s")
        );
        assert!(discover_run_identities_for_runs_root(&runtime_root, &runs_root).is_err());
        assert_eq!(
            read_run_identity(&runtime_root, &run_root).unwrap(),
            Some(identity)
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir()
            .join("humanize-plugin-private-state")
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

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn legacy_identity(
        public_run_root: &Path,
        runs_root: &Path,
        run_id: &str,
    ) -> PrivateRunIdentity {
        PrivateRunIdentity {
            schema: RUN_IDENTITY_SCHEMA.to_string(),
            run_id: run_id.to_string(),
            public_run_root: std::path::absolute(public_run_root).unwrap(),
            runs_root: std::path::absolute(runs_root).unwrap(),
        }
    }

    fn legacy_private_run_root(runtime_root: &Path, public_run_root: &Path) -> PathBuf {
        let public_run_root = std::path::absolute(public_run_root).unwrap();
        runtime_root.join(format!(
            "r{:016x}",
            stable_hash(&public_run_root.to_string_lossy())
        ))
    }

    fn write_identity(private_run_root: &Path, identity: &PrivateRunIdentity) {
        fs::create_dir_all(private_run_root).unwrap();
        set_mode(private_run_root, 0o700);
        let path = private_run_root.join("identity.json");
        fs::write(&path, serde_json::to_vec(identity).unwrap()).unwrap();
        set_mode(&path, 0o600);
    }

    fn stable_hash(input: &str) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in input.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }
}
