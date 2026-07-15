use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use super::journal;
use super::public_metadata::redact_public_manifest_secrets;
use super::{
    RunAssetError, RunAssetManifest, SelectedRunAssetSink, atomic_write_private, create_dir_all,
    ensure_private_dir, write_create_new_private,
};

pub(super) fn existing_run_storage_error(run_id: &str, run_root: &Path) -> RunAssetError {
    let manifest_path = run_root.join("manifest.json");
    if matches!(
        fs::symlink_metadata(&manifest_path),
        Ok(metadata) if metadata.file_type().is_symlink()
    ) {
        return RunAssetError::new(format!(
            "run asset manifest {} is a symlink",
            manifest_path.display()
        ));
    }
    let raw_run_id = fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
        .and_then(|manifest| {
            manifest
                .get("storage")
                .and_then(|storage| storage.get("raw_run_id"))
                .or_else(|| manifest.get("run_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    if raw_run_id.as_deref().is_some_and(|raw| raw != run_id) {
        return RunAssetError::new(format!(
            "run asset storage hash collision for run id {run_id}: {} stores raw id {}",
            run_root.display(),
            raw_run_id.unwrap()
        ));
    }
    RunAssetError::new(format!(
        "run asset storage already exists for run id {run_id}: {}",
        run_root.display()
    ))
}

pub(super) fn selected_runtime_sink() -> SelectedRunAssetSink {
    if let Some(path) = env::var_os("HUMANIZE_RUNS_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        return SelectedRunAssetSink::HumanizeRunsDir(path);
    }

    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join("humanize-home"));
    SelectedRunAssetSink::CacheHome(home)
}

pub(super) fn write_manifest_file(manifest: &RunAssetManifest) -> Result<(), RunAssetError> {
    if let Some(parent) = manifest.manifest_path.parent() {
        create_dir_all(parent)?;
        ensure_private_dir(parent)?;
    }
    journal::seal_if_complete(manifest)?;
    let payload = serde_json::to_string_pretty(&manifest_disk_value(manifest)?)
        .map_err(|err| RunAssetError::new(format!("serialize run asset manifest failed: {err}")))?;
    atomic_write_private(&manifest.manifest_path, payload.as_bytes()).map_err(|err| {
        RunAssetError::new(format!(
            "write run asset manifest {} failed: {err}",
            manifest.manifest_path.display()
        ))
    })
}

pub(super) fn write_public_projection_file(
    manifest: &RunAssetManifest,
) -> Result<(), RunAssetError> {
    if let Some(parent) = manifest.manifest_path.parent() {
        create_dir_all(parent)?;
        ensure_private_dir(parent)?;
    }
    journal::seal_if_complete(manifest)?;
    let payload = serde_json::to_vec_pretty(&journal::public_manifest_projection(manifest)?)
        .map_err(|err| RunAssetError::new(format!("serialize public manifest failed: {err}")))?;
    atomic_write_private(&manifest.manifest_path, &payload).map_err(|err| {
        RunAssetError::new(format!(
            "write public manifest {} failed: {err}",
            manifest.manifest_path.display()
        ))
    })
}

pub(super) fn write_manifest_file_create_new(
    manifest: &RunAssetManifest,
) -> Result<(), RunAssetError> {
    if let Some(parent) = manifest.manifest_path.parent() {
        create_dir_all(parent)?;
        ensure_private_dir(parent)?;
    }
    journal::seal_if_complete(manifest)?;
    let payload = serde_json::to_string_pretty(&manifest_disk_value(manifest)?)
        .map_err(|err| RunAssetError::new(format!("serialize run asset manifest failed: {err}")))?;
    write_create_new_private(&manifest.manifest_path, payload.as_bytes()).map_err(|err| {
        RunAssetError::new(format!(
            "create run asset manifest {} failed: {err}",
            manifest.manifest_path.display()
        ))
    })
}

pub(super) fn write_private_manifest_file(
    path: &Path,
    manifest: &RunAssetManifest,
    create_new: bool,
) -> Result<(), RunAssetError> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(|err| {
        RunAssetError::new(format!(
            "serialize private run asset manifest failed: {err}"
        ))
    })?;
    bytes.push(b'\n');
    if create_new {
        write_create_new_private(path, &bytes)
    } else {
        atomic_write_private(path, &bytes)
    }
}

pub(super) fn manifest_disk_value(
    manifest: &RunAssetManifest,
) -> Result<serde_json::Value, RunAssetError> {
    let mut value = serde_json::to_value(manifest)
        .map_err(|err| RunAssetError::new(format!("serialize run asset manifest failed: {err}")))?;
    if let serde_json::Value::Object(object) = &mut value {
        object.insert("journal".to_string(), journal::manifest_summary(manifest)?);
        redact_public_manifest_secrets(object);
    }
    Ok(value)
}
