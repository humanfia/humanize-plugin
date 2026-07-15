use std::path::{Path, PathBuf};

use super::durable_fs::{read_regular_private, validate_private_dir_path};
use super::{
    RunAssetError, RunAssetManifest, allocation_capture_dir, relative_path_string, storage_segment,
};

pub(crate) fn read_manifest_for_run_root(
    run_root: &Path,
    expected_run_id: Option<&str>,
) -> Result<RunAssetManifest, RunAssetError> {
    validate_private_dir_path(run_root)?;
    let path = run_root.join("manifest.json");
    let Some(bytes) = read_regular_private(&path)? else {
        return Err(RunAssetError::new(format!(
            "run asset manifest {} does not exist",
            path.display()
        )));
    };
    let manifest = serde_json::from_slice::<RunAssetManifest>(&bytes).map_err(|err| {
        RunAssetError::new(format!(
            "parse run asset manifest {} failed: {err}",
            path.display()
        ))
    })?;
    validate_manifest_layout(&manifest, run_root, run_root, expected_run_id)?;
    Ok(manifest)
}

pub(super) fn validate_manifest_layout(
    manifest: &RunAssetManifest,
    expected_root: &Path,
    capture_root: &Path,
    expected_run_id: Option<&str>,
) -> Result<(), RunAssetError> {
    validate_private_dir_path(expected_root)?;
    if expected_run_id.is_some_and(|run_id| manifest.run_id != run_id) {
        return Err(authority_mismatch("run identity"));
    }
    let expected_storage = storage_segment("run", &manifest.run_id);
    require_equal(
        "storage raw run identity",
        &manifest.storage.raw_run_id,
        &manifest.run_id,
    )?;
    require_equal(
        "storage directory path",
        &manifest.storage.run_directory,
        &expected_storage,
    )?;
    require_equal(
        "storage relative path",
        &manifest.storage.run_relative_path,
        &expected_storage,
    )?;
    require_equal("root path", &manifest.root, &expected_root.to_path_buf())?;

    let manifest_path = expected_root.join("manifest.json");
    require_equal("manifest path", &manifest.manifest_path, &manifest_path)?;
    require_equal(
        "artifact manifest path",
        &manifest.artifact_paths.manifest,
        &manifest_path,
    )?;
    require_equal(
        "artifact manifest relative path",
        &manifest.artifact_paths.manifest_relative_path,
        &"manifest.json".to_string(),
    )?;

    let mut revision_paths = Vec::new();
    let mut revision_relative_paths = Vec::new();
    for (index, revision) in manifest.flow.revisions.iter().enumerate() {
        let expected_revision_id = format!("rev-{:04}", index + 1);
        require_equal(
            "flow revision identity",
            &revision.revision_id,
            &expected_revision_id,
        )?;
        let path = expected_root
            .join("flow")
            .join("revisions")
            .join(&expected_revision_id)
            .join("flow-lock.json");
        let relative = relative_path_string(expected_root, &path);
        require_equal("flow revision path", &revision.export_path, &path)?;
        require_equal(
            "flow revision relative path",
            &revision.relative_path,
            &relative,
        )?;
        revision_paths.push(path);
        revision_relative_paths.push(relative);
    }
    require_equal(
        "artifact flow revision paths",
        &manifest.artifact_paths.flow_revisions,
        &revision_paths,
    )?;
    require_equal(
        "artifact flow revision relative paths",
        &manifest.artifact_paths.flow_revision_relative_paths,
        &revision_relative_paths,
    )?;

    let current = manifest
        .flow
        .current_revision_id
        .as_ref()
        .map(|revision_id| {
            manifest
                .flow
                .revisions
                .iter()
                .position(|revision| &revision.revision_id == revision_id)
                .map(|index| {
                    (
                        revision_paths[index].clone(),
                        revision_relative_paths[index].clone(),
                    )
                })
                .ok_or_else(|| authority_mismatch("current flow revision identity"))
        });
    let current = current.transpose()?;
    let expected_current_path = current.as_ref().map(|(path, _)| path.clone());
    let expected_current_relative = current.as_ref().map(|(_, relative)| relative.clone());
    require_equal(
        "current flow revision path",
        &manifest.flow.current_export_path,
        &expected_current_path,
    )?;
    require_equal(
        "current flow revision relative path",
        &manifest.flow.current_export_relative_path,
        &expected_current_relative,
    )?;
    require_equal(
        "artifact current flow path",
        &manifest.artifact_paths.flow_current,
        &expected_current_path,
    )?;
    require_equal(
        "artifact current flow relative path",
        &manifest.artifact_paths.flow_current_relative_path,
        &expected_current_relative,
    )?;

    for (activation_id, activation) in &manifest.activations {
        require_equal(
            "activation identity",
            &activation.activation_id,
            activation_id,
        )?;
        require_equal(
            "activation run identity",
            &activation.run_id,
            &manifest.run_id,
        )?;
        let activation_root = capture_root
            .join("activations")
            .join(storage_segment("act", activation_id));
        let directory = allocation_capture_dir(&activation_root, activation.allocation_generation);
        let metadata_path = directory.join("metadata.json");
        let pipe_path = directory.join("transcript.pipe.log");
        let final_capture_path = directory.join("final-capture.txt");
        validate_activation_path(
            capture_root,
            "activation metadata path",
            &activation.metadata_path,
            &activation.relative_paths.metadata,
            &metadata_path,
        )?;
        validate_activation_path(
            capture_root,
            "activation transcript path",
            &activation.pipe_path,
            &activation.relative_paths.transcript_pipe,
            &pipe_path,
        )?;
        validate_activation_path(
            capture_root,
            "activation final capture path",
            &activation.final_capture_path,
            &activation.relative_paths.final_capture,
            &final_capture_path,
        )?;
    }
    Ok(())
}

fn validate_activation_path(
    root: &Path,
    label: &str,
    actual_path: &PathBuf,
    actual_relative: &str,
    expected_path: &PathBuf,
) -> Result<(), RunAssetError> {
    require_equal(label, actual_path, expected_path)?;
    require_equal(
        &format!("{label} relative path"),
        &actual_relative.to_string(),
        &relative_path_string(root, expected_path),
    )
}

fn require_equal<T: PartialEq>(label: &str, actual: &T, expected: &T) -> Result<(), RunAssetError> {
    if actual == expected {
        Ok(())
    } else {
        Err(authority_mismatch(label))
    }
}

fn authority_mismatch(label: &str) -> RunAssetError {
    RunAssetError::new(format!("run asset authority path mismatch: {label}"))
}
