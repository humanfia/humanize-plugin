use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runtime;

use super::{
    PublicRecordBatch, RunAssetError, RunAssetManifest, atomic_write_private,
    ensure_private_directory, read_private_directory, read_regular_private, remove_regular_private,
    write_create_new_private,
};

const OUTBOX_DIR: &str = "publication-outbox";
const PUBLISHED_DIR: &str = "publication-ledger";
const SEQUENCE_FILE: &str = "publication-sequence";
const PUBLICATION_SCHEMA: &str = "humanize.driver.publication.v2";

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PublicationMutation {
    RuntimeEvents {
        base_event_count: usize,
        events: Vec<runtime::Event>,
    },
    RunAssetManifest {
        base_manifest_sha256: Option<String>,
        manifest: Box<RunAssetManifest>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PublicationTransaction {
    schema: String,
    transaction_id: String,
    ordinal: u64,
    mutation: PublicationMutation,
    public_records: PublicRecordBatch,
}

impl PublicationTransaction {
    pub(crate) fn runtime(
        base_event_count: usize,
        events: Vec<runtime::Event>,
        public_records: PublicRecordBatch,
    ) -> Result<Self, RunAssetError> {
        if events.is_empty() {
            return Err(RunAssetError::new(
                "runtime publication transaction requires events",
            ));
        }
        Self::new(
            PublicationMutation::RuntimeEvents {
                base_event_count,
                events,
            },
            public_records,
        )
    }

    pub(crate) fn run_asset_manifest(
        base_manifest_sha256: Option<String>,
        manifest: RunAssetManifest,
        public_records: PublicRecordBatch,
    ) -> Result<Self, RunAssetError> {
        Self::new(
            PublicationMutation::RunAssetManifest {
                base_manifest_sha256,
                manifest: Box::new(manifest),
            },
            public_records,
        )
    }

    fn new(
        mutation: PublicationMutation,
        public_records: PublicRecordBatch,
    ) -> Result<Self, RunAssetError> {
        let transaction_id = transaction_id(&mutation, &public_records)?;
        Ok(Self {
            schema: PUBLICATION_SCHEMA.to_string(),
            transaction_id,
            ordinal: 0,
            mutation,
            public_records,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), RunAssetError> {
        if self.schema != PUBLICATION_SCHEMA || self.ordinal == 0 {
            return Err(RunAssetError::new(
                "private publication transaction is malformed",
            ));
        }
        if self.transaction_id != transaction_id(&self.mutation, &self.public_records)? {
            return Err(RunAssetError::new(
                "private publication transaction identity mismatch",
            ));
        }
        match &self.mutation {
            PublicationMutation::RuntimeEvents { events, .. } if events.is_empty() => Err(
                RunAssetError::new("runtime publication transaction requires events"),
            ),
            PublicationMutation::RunAssetManifest { manifest, .. }
                if manifest.run_id.is_empty() =>
            {
                Err(RunAssetError::new(
                    "run asset publication transaction requires a run id",
                ))
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn mutation(&self) -> &PublicationMutation {
        &self.mutation
    }

    pub(crate) fn public_records(&self) -> &PublicRecordBatch {
        &self.public_records
    }

    pub(crate) fn ordinal(&self) -> u64 {
        self.ordinal
    }

    fn public_source_native_ids(&self) -> impl Iterator<Item = &str> {
        self.public_records.source_native_ids()
    }

    fn file_name(&self) -> String {
        format!(
            "{:020}-{}.json",
            self.ordinal,
            self.transaction_id
                .strip_prefix("publication-sha256:")
                .unwrap_or(&self.transaction_id)
        )
    }
}

pub(crate) fn persist_pending(
    private_run_root: &Path,
    mut transaction: PublicationTransaction,
) -> Result<PublicationTransaction, RunAssetError> {
    fail_if_marker_exists(
        "HUMANIZE_DRIVER_FAIL_OUTBOX_IF_EXISTS",
        "injected publication outbox persistence failure",
    )?;
    if !pending_transactions(private_run_root)?.is_empty() {
        return Err(RunAssetError::new(
            "pending public publication must reconcile before mutation",
        ));
    }
    transaction.ordinal = next_ordinal(private_run_root)?;
    transaction.validate()?;
    let path = pending_path(private_run_root, &transaction);
    let parent = path
        .parent()
        .ok_or_else(|| RunAssetError::new("private publication outbox path has no parent"))?;
    ensure_private_directory(parent)?;
    let mut bytes = serde_json::to_vec_pretty(&transaction).map_err(|err| {
        RunAssetError::new(format!(
            "serialize private publication transaction failed: {err}"
        ))
    })?;
    bytes.push(b'\n');
    write_create_new_private(&path, &bytes)?;
    Ok(transaction)
}

pub(crate) fn acknowledge(
    private_run_root: &Path,
    transaction: &PublicationTransaction,
) -> Result<(), RunAssetError> {
    fail_if_marker_exists(
        "HUMANIZE_DRIVER_FAIL_OUTBOX_ACK_IF_EXISTS",
        "injected publication acknowledgement failure",
    )?;
    let source = pending_path(private_run_root, transaction);
    let destination = published_path(private_run_root, transaction);
    if let Some(existing) = read_regular_private(&destination)? {
        let expected = read_regular_private(&source)?.unwrap_or_default();
        if !expected.is_empty() && existing != expected {
            return Err(RunAssetError::new(
                "published transaction conflicts during acknowledgement",
            ));
        }
        if source.exists() {
            fs::remove_file(&source).map_err(|err| {
                RunAssetError::new(format!(
                    "remove acknowledged publication {} failed: {err}",
                    source.display()
                ))
            })?;
        }
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        ensure_private_directory(parent)?;
    }
    fs::rename(&source, &destination).map_err(|err| {
        RunAssetError::new(format!(
            "acknowledge publication {} failed: {err}",
            source.display()
        ))
    })?;
    sync_parent(&source)?;
    sync_parent(&destination)
}

pub(crate) fn pending_transactions(
    private_run_root: &Path,
) -> Result<Vec<PublicationTransaction>, RunAssetError> {
    transaction_files(&private_run_root.join("driver").join(OUTBOX_DIR))
}

pub(crate) fn published_transactions(
    private_run_root: &Path,
) -> Result<Vec<PublicationTransaction>, RunAssetError> {
    transaction_files(&private_run_root.join("driver").join(PUBLISHED_DIR))
}

pub(crate) fn published_source_native_ids(
    private_run_root: &Path,
) -> Result<BTreeSet<String>, RunAssetError> {
    let mut sources = BTreeSet::new();
    for transaction in published_transactions(private_run_root)? {
        sources.extend(transaction.public_source_native_ids().map(str::to_string));
    }
    Ok(sources)
}

pub(crate) fn fail_public_event_if_requested() -> Result<(), RunAssetError> {
    fail_if_marker_exists(
        "HUMANIZE_DRIVER_FAIL_PUBLIC_EVENT_IF_EXISTS",
        "injected public event publication failure",
    )
}

pub(crate) fn manifest_sha256(manifest: &RunAssetManifest) -> Result<String, RunAssetError> {
    let bytes = serde_json::to_vec(manifest).map_err(|err| {
        RunAssetError::new(format!(
            "serialize private run asset identity failed: {err}"
        ))
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn transaction_id(
    mutation: &PublicationMutation,
    public_records: &PublicRecordBatch,
) -> Result<String, RunAssetError> {
    let bytes =
        serde_json::to_vec(&(PUBLICATION_SCHEMA, mutation, public_records)).map_err(|err| {
            RunAssetError::new(format!("serialize publication identity failed: {err}"))
        })?;
    Ok(format!("publication-sha256:{:x}", Sha256::digest(bytes)))
}

fn next_ordinal(private_run_root: &Path) -> Result<u64, RunAssetError> {
    let path = private_run_root.join("driver").join(SEQUENCE_FILE);
    let current = match read_regular_private(&path)? {
        Some(bytes) => std::str::from_utf8(&bytes)
            .map_err(|_| RunAssetError::new("private publication sequence is not UTF-8"))?
            .trim()
            .parse::<u64>()
            .map_err(|_| RunAssetError::new("private publication sequence is malformed"))?,
        None => highest_existing_ordinal(private_run_root)?,
    };
    let next = current
        .checked_add(1)
        .ok_or_else(|| RunAssetError::new("private publication sequence overflow"))?;
    atomic_write_private(&path, format!("{next}\n").as_bytes())?;
    Ok(next)
}

fn highest_existing_ordinal(private_run_root: &Path) -> Result<u64, RunAssetError> {
    Ok(pending_transactions(private_run_root)?
        .into_iter()
        .chain(published_transactions(private_run_root)?)
        .map(|transaction| transaction.ordinal())
        .max()
        .unwrap_or(0))
}

fn transaction_files(path: &Path) -> Result<Vec<PublicationTransaction>, RunAssetError> {
    let Some(entries) = read_private_directory(path)? else {
        return Ok(Vec::new());
    };
    let mut transactions = Vec::new();
    for (name, bytes) in entries {
        let transaction =
            serde_json::from_slice::<PublicationTransaction>(&bytes).map_err(|err| {
                RunAssetError::new(format!(
                    "parse private publication transaction {} failed: {err}",
                    name.to_string_lossy()
                ))
            })?;
        transaction.validate()?;
        let transaction_name = transaction.file_name();
        if name.to_string_lossy() == transaction_name {
            transactions.push(transaction);
            continue;
        }
        if name
            .to_str()
            .is_some_and(|name| interrupted_atomic_create_name(name, &transaction_name))
        {
            remove_regular_private(&path.join(&name))?;
            continue;
        }
        return Err(RunAssetError::new(
            "private publication transaction filename does not match its identity",
        ));
    }
    transactions.sort_by_key(PublicationTransaction::ordinal);
    for pair in transactions.windows(2) {
        if pair[0].ordinal() == pair[1].ordinal() {
            return Err(RunAssetError::new(
                "private publication transaction ordinal is duplicated",
            ));
        }
    }
    Ok(transactions)
}

fn interrupted_atomic_create_name(name: &str, target: &str) -> bool {
    let Some(suffix) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_prefix(target))
        .and_then(|name| name.strip_prefix(".tmp-"))
    else {
        return false;
    };
    let Some((process_id, sequence)) = suffix.split_once('-') else {
        return false;
    };
    !process_id.is_empty()
        && !sequence.is_empty()
        && process_id.bytes().all(|byte| byte.is_ascii_digit())
        && sequence.bytes().all(|byte| byte.is_ascii_digit())
}

fn pending_path(private_run_root: &Path, transaction: &PublicationTransaction) -> PathBuf {
    private_run_root
        .join("driver")
        .join(OUTBOX_DIR)
        .join(transaction.file_name())
}

fn published_path(private_run_root: &Path, transaction: &PublicationTransaction) -> PathBuf {
    private_run_root
        .join("driver")
        .join(PUBLISHED_DIR)
        .join(transaction.file_name())
}

fn fail_if_marker_exists(variable: &str, message: &str) -> Result<(), RunAssetError> {
    if let Some(path) = std::env::var_os(variable)
        && PathBuf::from(path).exists()
    {
        return Err(RunAssetError::new(message));
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), RunAssetError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|err| {
            RunAssetError::new(format!(
                "sync private publication directory {} failed: {err}",
                parent.display()
            ))
        })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{PUBLISHED_DIR, published_transactions};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn recovery_rejects_dangling_symlinked_publication_ledger() {
        let root = test_root("dangling-ledger");
        let private_run_root = root.join("private-run");
        let driver_dir = private_run_root.join("driver");
        fs::create_dir_all(&driver_dir).unwrap();
        set_mode(&private_run_root, 0o700);
        set_mode(&driver_dir, 0o700);
        let outside = root.join("outside-ledger");
        symlink(&outside, driver_dir.join(PUBLISHED_DIR)).unwrap();

        let result = published_transactions(&private_run_root);

        assert!(result.is_err(), "dangling ledger symlink was accepted");
        assert!(
            !outside.exists(),
            "publication recovery created outside state"
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir()
            .join("humanize-plugin-publication-safety")
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
}
