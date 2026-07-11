use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::flow::{self, FlowCheckMode, FlowExportFormat, FlowLock};
use crate::pipe_sink::PipeSinkIdentity;

mod durable_fs;
mod fault;
mod records;

use fault::RunAssetFaultKind;
pub use fault::{RunAssetFault, RunAssetFaultPoint};
pub use records::{
    ActivationProbeState, HookFactInput, RunAssetRecordFile, RunAssetRecordIndex,
    RunAssetSessionIndex, RunAssetSessionRelation, SessionRelation, TopologyDecisionInput,
};

pub const RUN_ASSET_PROTOCOL_VERSION: &str = "2024-11-05";
pub const RUN_ASSET_PACKAGE_NAME: &str = "humanize-plugin";
const STORAGE_SEGMENT_MAX_BYTES: usize = 180;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetStore {
    sink: RunAssetSink,
    clock: RunAssetClock,
    fault: Option<RunAssetFault>,
}

impl Default for RunAssetStore {
    fn default() -> Self {
        Self::runtime_default()
    }
}

impl RunAssetStore {
    pub fn runtime_default() -> Self {
        Self::new(RunAssetSink::Auto)
    }

    pub fn new(sink: RunAssetSink) -> Self {
        Self {
            sink,
            clock: RunAssetClock::Realtime,
            fault: None,
        }
    }

    pub fn new_with_fixed_clock(sink: RunAssetSink, timestamp_ms: u64) -> Self {
        Self {
            sink,
            clock: RunAssetClock::Fixed(timestamp_ms),
            fault: None,
        }
    }

    #[doc(hidden)]
    pub fn new_with_fault(sink: RunAssetSink, fault: RunAssetFault) -> Self {
        Self {
            sink,
            clock: RunAssetClock::Realtime,
            fault: Some(fault),
        }
    }

    #[doc(hidden)]
    pub fn new_with_resource_cleanup_fault_once(
        sink: RunAssetSink,
        activation_id: impl Into<String>,
    ) -> Self {
        Self {
            sink,
            clock: RunAssetClock::Realtime,
            fault: Some(RunAssetFault::resource_cleanup_once(activation_id)),
        }
    }

    pub fn run_root(&self, run_id: &str) -> Result<PathBuf, RunAssetError> {
        let safe_run_id = storage_segment("run", run_id);
        match self.selected_sink() {
            SelectedRunAssetSink::HumanizeRunsDir(path) => Ok(path.join(safe_run_id)),
            SelectedRunAssetSink::CacheHome(path) => Ok(path
                .join(".cache")
                .join("humanize")
                .join("runs")
                .join(safe_run_id)),
            SelectedRunAssetSink::Root(path) => Ok(path.join(safe_run_id)),
        }
    }

    pub fn start_run_manifest(&self, run_id: &str) -> Result<RunAssetManifest, RunAssetError> {
        let safe_run_id = storage_segment("run", run_id);
        let run_root = self.run_root(run_id)?;
        match fs::symlink_metadata(&run_root) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(RunAssetError::new(format!(
                    "run asset storage {} is a symlink",
                    run_root.display()
                )));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(RunAssetError::new(format!(
                    "run asset storage {} is not a directory",
                    run_root.display()
                )));
            }
            Ok(_) => {
                return Err(existing_run_storage_error(run_id, &run_root));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(RunAssetError::new(format!(
                    "inspect run asset storage {} failed: {err}",
                    run_root.display()
                )));
            }
        }
        create_dir_all(&run_root)?;
        ensure_private_dir(&run_root)?;
        let manifest_path = run_root.join("manifest.json");
        let now = self.now_ms();
        let mut manifest = RunAssetManifest {
            version: 1,
            run_id: run_id.to_string(),
            created_at_ms: now,
            updated_at_ms: now,
            sink: self.selected_sink().name().to_string(),
            root: run_root,
            manifest_path: manifest_path.clone(),
            storage: RunAssetStorage {
                raw_run_id: run_id.to_string(),
                run_directory: safe_run_id.clone(),
                run_relative_path: safe_run_id,
            },
            protocol: RunAssetProtocol {
                mcp_protocol_version: RUN_ASSET_PROTOCOL_VERSION.to_string(),
                package_name: RUN_ASSET_PACKAGE_NAME.to_string(),
                package_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            flow: RunAssetFlow {
                main_flow: true,
                status: "pending".to_string(),
                complete: false,
                current_revision_id: None,
                current_export_path: None,
                current_export_relative_path: None,
                revisions: Vec::new(),
            },
            artifact_paths: RunAssetArtifactPaths {
                manifest: manifest_path,
                manifest_relative_path: "manifest.json".to_string(),
                flow_current: None,
                flow_current_relative_path: None,
                flow_revisions: Vec::new(),
                flow_revision_relative_paths: Vec::new(),
            },
            activations: BTreeMap::new(),
            preservation_errors: Vec::new(),
            preservation_blocked: false,
            completion: RunAssetCompletion::default(),
        };
        refresh_completion(&mut manifest);
        records::record_manifest_started(&mut manifest, now)?;
        write_manifest_file_create_new(&manifest)?;
        Ok(manifest)
    }

    pub fn persist_flow_package(
        &self,
        run_id: &str,
        lock: &FlowLock,
        content_hash: &str,
        review_status: &str,
    ) -> Result<RunAssetManifest, RunAssetError> {
        let mut manifest = self.start_run_manifest(run_id)?;
        self.persist_flow_revision(&mut manifest, lock, content_hash, review_status)?;
        Ok(manifest)
    }

    pub fn persist_flow_revision(
        &self,
        manifest: &mut RunAssetManifest,
        lock: &FlowLock,
        content_hash: &str,
        review_status: &str,
    ) -> Result<RunAssetFlowRevision, RunAssetError> {
        let revision = self.prepare_flow_revision(manifest, lock, content_hash, review_status)?;
        self.commit_flow_revision_applied(manifest, &revision.revision_id)
    }

    pub fn prepare_flow_revision(
        &self,
        manifest: &mut RunAssetManifest,
        lock: &FlowLock,
        content_hash: &str,
        review_status: &str,
    ) -> Result<RunAssetFlowRevision, RunAssetError> {
        let revision_id = format!("rev-{:04}", manifest.flow.revisions.len() + 1);
        let revision_path = manifest
            .root
            .join("flow")
            .join("revisions")
            .join(&revision_id)
            .join("flow-lock.json");
        if let Some(parent) = revision_path.parent() {
            create_dir_all(parent)?;
            ensure_private_dir(parent)?;
        }

        let exported = flow::flow_export(lock, FlowExportFormat::Json);
        atomic_write_private(&revision_path, exported.as_bytes()).map_err(|err| {
            RunAssetError::new(format!(
                "write flow export {} failed: {err}",
                revision_path.display()
            ))
        })?;

        let now = self.now_ms();
        let revision = RunAssetFlowRevision {
            revision_id: revision_id.clone(),
            main_flow: true,
            flow_lock_id: lock.id().to_string(),
            content_hash: content_hash.to_string(),
            review_status: review_status.to_string(),
            flow_lock_mode: flow_check_mode_name(lock.mode()).to_string(),
            export_format: "json".to_string(),
            export_path: revision_path.clone(),
            relative_path: relative_path_string(&manifest.root, &revision_path),
            created_at_ms: now,
            apply_state: "prepared".to_string(),
            applied_at_ms: None,
        };

        let mut candidate = manifest.clone();
        candidate.flow.status = "prepared".to_string();
        candidate.flow.complete = false;
        candidate.flow.current_revision_id = None;
        candidate.flow.current_export_path = None;
        candidate.flow.current_export_relative_path = None;
        candidate.flow.revisions.push(revision.clone());
        candidate.artifact_paths.flow_current = None;
        candidate.artifact_paths.flow_current_relative_path = None;
        candidate
            .artifact_paths
            .flow_revisions
            .push(revision_path.clone());
        candidate
            .artifact_paths
            .flow_revision_relative_paths
            .push(relative_path_string(&manifest.root, &revision_path));
        candidate.updated_at_ms = now;
        refresh_completion(&mut candidate);
        records::record_flow_revision(&mut candidate, &revision, "prepared", now)?;
        write_manifest_file(&candidate)?;
        *manifest = candidate;
        Ok(revision)
    }

    pub fn commit_flow_revision_applied(
        &self,
        manifest: &mut RunAssetManifest,
        revision_id: &str,
    ) -> Result<RunAssetFlowRevision, RunAssetError> {
        let now = self.now_ms();
        let mut candidate = manifest.clone();
        let Some(index) = candidate
            .flow
            .revisions
            .iter()
            .position(|revision| revision.revision_id == revision_id)
        else {
            return Err(RunAssetError::new(format!(
                "flow revision was not prepared: {revision_id}"
            )));
        };
        let revision_path = candidate.flow.revisions[index].export_path.clone();
        let revision_relative_path = candidate.flow.revisions[index].relative_path.clone();
        candidate.flow.revisions[index].apply_state = "applied".to_string();
        candidate.flow.revisions[index].applied_at_ms = Some(now);
        candidate.flow.status = "complete".to_string();
        candidate.flow.complete = true;
        candidate.flow.current_revision_id = Some(revision_id.to_string());
        candidate.flow.current_export_path = Some(revision_path.clone());
        candidate.flow.current_export_relative_path = Some(revision_relative_path.clone());
        candidate.artifact_paths.flow_current = Some(revision_path);
        candidate.artifact_paths.flow_current_relative_path = Some(revision_relative_path);
        candidate.updated_at_ms = now;
        refresh_completion(&mut candidate);
        let revision = candidate.flow.revisions[index].clone();
        records::record_flow_revision(&mut candidate, &revision, "applied", now)?;
        write_manifest_file(&candidate)?;
        *manifest = candidate;
        Ok(revision)
    }

    pub fn mark_flow_revision_failed(
        &self,
        manifest: &mut RunAssetManifest,
        revision_id: &str,
        error: &str,
    ) -> Result<(), RunAssetError> {
        let now = self.now_ms();
        let mut candidate = manifest.clone();
        if let Some(revision) = candidate
            .flow
            .revisions
            .iter_mut()
            .find(|revision| revision.revision_id == revision_id)
        {
            revision.apply_state = "failed".to_string();
        }
        candidate.flow.status = "failed".to_string();
        candidate.flow.complete = false;
        candidate
            .preservation_errors
            .push(RunAssetPreservationError {
                activation_id: None,
                stage: "flow_package".to_string(),
                error: error.to_string(),
                recorded_at_ms: now,
            });
        candidate.preservation_blocked = true;
        candidate.updated_at_ms = now;
        refresh_completion(&mut candidate);
        let preservation_error = candidate
            .preservation_errors
            .last()
            .expect("pushed preservation error should exist")
            .clone();
        records::record_preservation_failure(&mut candidate, &preservation_error)?;
        if let Some(revision) = candidate
            .flow
            .revisions
            .iter()
            .find(|revision| revision.revision_id == revision_id)
            .cloned()
        {
            records::record_flow_revision(&mut candidate, &revision, "failed", now)?;
        }
        write_manifest_file(&candidate)?;
        *manifest = candidate;
        Ok(())
    }

    pub fn start_activation_capture(
        &self,
        manifest: &mut RunAssetManifest,
        update: RunAssetActivationUpdate,
    ) -> Result<RunAssetActivationPaths, RunAssetError> {
        self.fail_if_fault(
            RunAssetFaultPoint::StartActivationTranscript,
            &update.activation_id,
        )?;
        let activation_dir = manifest
            .root
            .join("activations")
            .join(storage_segment("act", &update.activation_id));
        create_dir_all(&activation_dir)?;
        ensure_private_dir(&activation_dir)?;
        let metadata_path = activation_dir.join("metadata.json");
        let pipe_path = activation_dir.join("transcript.pipe.log");
        let final_capture_path = activation_dir.join("final-capture.txt");
        let pipe_identity = open_pipe_log_private(&pipe_path)?;
        let pipe_relative_path = relative_path_string(&manifest.root, &pipe_path);

        let now = self.now_ms();
        let activation_id = update.activation_id.clone();
        let activation = RunAssetActivation {
            run_id: manifest.run_id.clone(),
            activation_id: update.activation_id,
            node_id: update.node_id,
            adapter: update.adapter,
            tmux_target: update.tmux.target(),
            session_id: update.tmux.session_id,
            window_id: update.tmux.window_id,
            window_name: update.tmux.window_name,
            pane_id: update.tmux.pane_id,
            metadata_path: metadata_path.clone(),
            pipe_path: pipe_path.clone(),
            final_capture_path: final_capture_path.clone(),
            relative_paths: RunAssetActivationRelativePaths {
                metadata: relative_path_string(&manifest.root, &metadata_path),
                transcript_pipe: relative_path_string(&manifest.root, &pipe_path),
                final_capture: relative_path_string(&manifest.root, &final_capture_path),
            },
            started_at_ms: now,
            ended_at_ms: None,
            termination_reason: update.termination_reason,
            capture_phase: "starting".to_string(),
            pipe_acknowledged: false,
            capture_complete: false,
            preservation_status: "starting".to_string(),
            resource_cleanup_status: "pending".to_string(),
            resource_cleanup_error: None,
        };
        self.fail_if_fault(RunAssetFaultPoint::StartActivationMetadata, &activation_id)?;
        write_activation_metadata_file(&activation)?;
        let mut candidate = manifest.clone();
        candidate
            .activations
            .insert(activation.activation_id.clone(), activation);
        candidate.updated_at_ms = now;
        refresh_completion(&mut candidate);
        let activation = candidate
            .activations
            .get(&activation_id)
            .expect("inserted activation should exist")
            .clone();
        records::record_tmux_activation(&mut candidate, &activation, "capture_started", now)?;
        records::record_activation_probe(
            &mut candidate,
            &activation.activation_id,
            &activation.node_id,
            ActivationProbeState::Ready,
            now,
        )?;
        records::record_session_relation(
            &mut candidate,
            &activation.session_id,
            SessionRelation::Executes,
            Some(&activation.activation_id),
            now,
        )?;
        self.fail_if_fault(RunAssetFaultPoint::StartActivationManifest, &activation_id)?;
        write_manifest_file(&candidate)?;
        *manifest = candidate;

        Ok(RunAssetActivationPaths {
            metadata_path,
            pipe_path,
            pipe_relative_path,
            pipe_identity,
            final_capture_path,
        })
    }

    pub fn register_expected_activation(
        &self,
        manifest: &mut RunAssetManifest,
        activation_id: &str,
        node_id: &str,
        adapter: &str,
    ) -> Result<(), RunAssetError> {
        if manifest.activations.contains_key(activation_id) {
            return Ok(());
        }
        self.fail_if_fault(
            RunAssetFaultPoint::RegisterExpectedActivation,
            activation_id,
        )?;
        let activation_dir = manifest
            .root
            .join("activations")
            .join(storage_segment("act", activation_id));
        create_dir_all(&activation_dir)?;
        ensure_private_dir(&activation_dir)?;
        let metadata_path = activation_dir.join("metadata.json");
        let pipe_path = activation_dir.join("transcript.pipe.log");
        let final_capture_path = activation_dir.join("final-capture.txt");
        let now = self.now_ms();
        let activation = RunAssetActivation {
            run_id: manifest.run_id.clone(),
            activation_id: activation_id.to_string(),
            node_id: node_id.to_string(),
            adapter: adapter.to_string(),
            tmux_target: String::new(),
            session_id: String::new(),
            window_id: String::new(),
            window_name: String::new(),
            pane_id: String::new(),
            metadata_path: metadata_path.clone(),
            pipe_path: pipe_path.clone(),
            final_capture_path: final_capture_path.clone(),
            relative_paths: RunAssetActivationRelativePaths {
                metadata: relative_path_string(&manifest.root, &metadata_path),
                transcript_pipe: relative_path_string(&manifest.root, &pipe_path),
                final_capture: relative_path_string(&manifest.root, &final_capture_path),
            },
            started_at_ms: now,
            ended_at_ms: None,
            termination_reason: None,
            capture_phase: "pending".to_string(),
            pipe_acknowledged: false,
            capture_complete: false,
            preservation_status: "pending".to_string(),
            resource_cleanup_status: "pending".to_string(),
            resource_cleanup_error: None,
        };
        write_activation_metadata_file(&activation)?;
        let mut candidate = manifest.clone();
        candidate
            .activations
            .insert(activation.activation_id.clone(), activation);
        candidate.updated_at_ms = now;
        refresh_completion(&mut candidate);
        records::record_activation_probe(
            &mut candidate,
            activation_id,
            node_id,
            ActivationProbeState::Planned,
            now,
        )?;
        write_manifest_file(&candidate)?;
        *manifest = candidate;
        Ok(())
    }

    pub fn mark_activation_capture_acknowledged(
        &self,
        manifest: &mut RunAssetManifest,
        activation_id: &str,
    ) -> Result<RunAssetActivation, RunAssetError> {
        let Some(existing) = manifest.activations.get(activation_id) else {
            return Err(RunAssetError::new(format!(
                "activation capture was not started: {activation_id}"
            )));
        };
        if existing.preservation_status == "failed" {
            return Err(RunAssetError::new(format!(
                "activation capture has failed: {activation_id}"
            )));
        }
        let now = self.now_ms();
        let mut candidate = manifest.clone();
        let activation = candidate
            .activations
            .get_mut(activation_id)
            .expect("checked activation should exist");
        activation.capture_phase = "capturing".to_string();
        activation.pipe_acknowledged = true;
        activation.preservation_status = "capturing".to_string();
        let activation = activation.clone();
        write_activation_metadata_file(&activation)?;
        candidate.updated_at_ms = now;
        refresh_completion(&mut candidate);
        records::record_tmux_activation(&mut candidate, &activation, "capture_acknowledged", now)?;
        write_manifest_file(&candidate)?;
        *manifest = candidate;
        Ok(activation)
    }

    pub fn complete_activation_capture(
        &self,
        manifest: &mut RunAssetManifest,
        activation_id: &str,
        termination_reason: &str,
        final_capture: &str,
    ) -> Result<RunAssetActivation, RunAssetError> {
        let Some(existing) = manifest.activations.get(activation_id) else {
            return Err(RunAssetError::new(format!(
                "activation capture was not started: {activation_id}"
            )));
        };
        if !activation_can_complete(existing) {
            return Err(RunAssetError::new(format!(
                "activation capture cannot complete before pipe start acknowledgement: {activation_id}"
            )));
        }
        atomic_write_private(&existing.final_capture_path, final_capture.as_bytes()).map_err(
            |err| {
                RunAssetError::new(format!(
                    "write final capture {} failed: {err}",
                    existing.final_capture_path.display()
                ))
            },
        )?;

        let now = self.now_ms();
        let mut candidate = manifest.clone();
        let activation = candidate
            .activations
            .get_mut(activation_id)
            .expect("checked activation should exist");
        activation.ended_at_ms = Some(now);
        activation.termination_reason = Some(termination_reason.to_string());
        activation.capture_phase = "complete".to_string();
        activation.capture_complete = true;
        activation.preservation_status = "complete".to_string();
        let activation = activation.clone();
        write_activation_metadata_file(&activation)?;
        candidate.updated_at_ms = now;
        refresh_completion(&mut candidate);
        records::record_tmux_activation(&mut candidate, &activation, "capture_completed", now)?;
        records::record_activation_probe(
            &mut candidate,
            &activation.activation_id,
            &activation.node_id,
            ActivationProbeState::Closed,
            now,
        )?;
        write_manifest_file(&candidate)?;
        *manifest = candidate;
        Ok(activation)
    }

    pub fn persist_activation_final_capture_snapshot(
        &self,
        manifest: &RunAssetManifest,
        activation_id: &str,
        final_capture: &str,
    ) -> Result<PathBuf, RunAssetError> {
        let activation = manifest.activations.get(activation_id).ok_or_else(|| {
            RunAssetError::new(format!(
                "activation capture was not started: {activation_id}"
            ))
        })?;
        atomic_write_private(&activation.final_capture_path, final_capture.as_bytes()).map_err(
            |err| {
                RunAssetError::new(format!(
                    "write final capture {} failed: {err}",
                    activation.final_capture_path.display()
                ))
            },
        )?;
        Ok(activation.final_capture_path.clone())
    }

    pub fn finalize_failed_activation_capture(
        &self,
        manifest: &mut RunAssetManifest,
        activation_id: &str,
        termination_reason: &str,
        final_capture: &str,
    ) -> Result<RunAssetActivation, RunAssetError> {
        let Some(existing) = manifest.activations.get(activation_id) else {
            return Err(RunAssetError::new(format!(
                "activation capture was not started: {activation_id}"
            )));
        };
        atomic_write_private(&existing.final_capture_path, final_capture.as_bytes()).map_err(
            |err| {
                RunAssetError::new(format!(
                    "write final capture {} failed: {err}",
                    existing.final_capture_path.display()
                ))
            },
        )?;

        let now = self.now_ms();
        let mut candidate = manifest.clone();
        let activation = candidate
            .activations
            .get_mut(activation_id)
            .expect("checked activation should exist");
        activation.ended_at_ms = Some(now);
        if activation.termination_reason.is_none() {
            activation.termination_reason = Some(termination_reason.to_string());
        }
        activation.capture_phase = "failed".to_string();
        activation.capture_complete = false;
        activation.preservation_status = "failed".to_string();
        let activation = activation.clone();
        write_activation_metadata_file(&activation)?;
        candidate.updated_at_ms = now;
        refresh_completion(&mut candidate);
        records::record_tmux_activation(&mut candidate, &activation, "capture_failed", now)?;
        records::record_activation_probe(
            &mut candidate,
            &activation.activation_id,
            &activation.node_id,
            ActivationProbeState::Suspended,
            now,
        )?;
        write_manifest_file(&candidate)?;
        *manifest = candidate;
        Ok(activation)
    }

    #[doc(hidden)]
    pub fn mark_activation_resource_cleanup(
        &self,
        manifest: &mut RunAssetManifest,
        activation_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<RunAssetActivation, RunAssetError> {
        self.fail_if_fault_kind(RunAssetFaultKind::ResourceCleanupStatus, activation_id)?;
        let now = self.now_ms();
        let mut candidate = manifest.clone();
        let activation = candidate
            .activations
            .get_mut(activation_id)
            .ok_or_else(|| {
                RunAssetError::new(format!(
                    "activation capture was not started: {activation_id}"
                ))
            })?;
        activation.resource_cleanup_status = status.to_string();
        activation.resource_cleanup_error = error.map(str::to_string);
        let activation = activation.clone();
        write_activation_metadata_file(&activation)?;
        candidate.updated_at_ms = now;
        refresh_completion(&mut candidate);
        records::record_tmux_activation(
            &mut candidate,
            &activation,
            &format!("resource_cleanup_{status}"),
            now,
        )?;
        write_manifest_file(&candidate)?;
        *manifest = candidate;
        Ok(activation)
    }

    pub fn record_preservation_error(
        &self,
        manifest: &mut RunAssetManifest,
        activation_id: Option<&str>,
        termination_reason: Option<&str>,
        stage: &str,
        error: &str,
    ) -> Result<(), RunAssetError> {
        let now = self.now_ms();
        manifest.updated_at_ms = now;
        let mut updated_activation = None;
        if let Some(activation_id) = activation_id {
            if let Some(activation) = manifest.activations.get_mut(activation_id) {
                activation.ended_at_ms = Some(now);
                if let Some(reason) = termination_reason {
                    activation.termination_reason = Some(reason.to_string());
                }
                activation.capture_phase = "failed".to_string();
                activation.capture_complete = false;
                activation.preservation_status = "failed".to_string();
                updated_activation = Some(activation.clone());
            }
        }
        manifest
            .preservation_errors
            .push(RunAssetPreservationError {
                activation_id: activation_id.map(str::to_string),
                stage: stage.to_string(),
                error: error.to_string(),
                recorded_at_ms: now,
            });
        manifest.preservation_blocked = true;
        let preservation_error = manifest
            .preservation_errors
            .last()
            .expect("pushed preservation error should exist")
            .clone();
        records::record_preservation_failure(manifest, &preservation_error)?;
        if let Some(activation_id) = activation_id
            && let Some(activation) = manifest.activations.get(activation_id).cloned()
        {
            records::record_activation_probe(
                manifest,
                &activation.activation_id,
                &activation.node_id,
                ActivationProbeState::Suspended,
                now,
            )?;
        }
        refresh_completion(manifest);
        let manifest_result = self.write_manifest(manifest);
        if let Some(activation) = updated_activation {
            let _ = write_activation_metadata_file(&activation);
        }
        manifest_result
    }

    pub fn record_activation_store_failure(
        &self,
        manifest: &mut RunAssetManifest,
        update: RunAssetActivationFailureUpdate,
    ) -> Result<(), RunAssetError> {
        let now = self.now_ms();
        let activation_dir = manifest
            .root
            .join("activations")
            .join(storage_segment("act", &update.activation_id));
        let metadata_path = activation_dir.join("metadata.json");
        let pipe_path = activation_dir.join("transcript.pipe.log");
        let final_capture_path = activation_dir.join("final-capture.txt");
        let mut candidate = manifest.clone();
        let activation = candidate
            .activations
            .entry(update.activation_id.clone())
            .or_insert_with(|| RunAssetActivation {
                run_id: manifest.run_id.clone(),
                activation_id: update.activation_id.clone(),
                node_id: update.node_id.clone(),
                adapter: update.adapter.clone(),
                tmux_target: update
                    .tmux
                    .as_ref()
                    .map(RunAssetTmuxTarget::target)
                    .unwrap_or_default(),
                session_id: update
                    .tmux
                    .as_ref()
                    .map(|target| target.session_id.clone())
                    .unwrap_or_default(),
                window_id: update
                    .tmux
                    .as_ref()
                    .map(|target| target.window_id.clone())
                    .unwrap_or_default(),
                window_name: update
                    .tmux
                    .as_ref()
                    .map(|target| target.window_name.clone())
                    .unwrap_or_default(),
                pane_id: update
                    .tmux
                    .as_ref()
                    .map(|target| target.pane_id.clone())
                    .unwrap_or_default(),
                metadata_path: metadata_path.clone(),
                pipe_path: pipe_path.clone(),
                final_capture_path: final_capture_path.clone(),
                relative_paths: RunAssetActivationRelativePaths {
                    metadata: relative_path_string(&manifest.root, &metadata_path),
                    transcript_pipe: relative_path_string(&manifest.root, &pipe_path),
                    final_capture: relative_path_string(&manifest.root, &final_capture_path),
                },
                started_at_ms: now,
                ended_at_ms: None,
                termination_reason: None,
                capture_phase: "pending".to_string(),
                pipe_acknowledged: false,
                capture_complete: false,
                preservation_status: "pending".to_string(),
                resource_cleanup_status: "pending".to_string(),
                resource_cleanup_error: None,
            });
        activation.node_id = update.node_id;
        activation.adapter = update.adapter;
        if let Some(tmux) = update.tmux {
            activation.tmux_target = tmux.target();
            activation.session_id = tmux.session_id;
            activation.window_id = tmux.window_id;
            activation.window_name = tmux.window_name;
            activation.pane_id = tmux.pane_id;
        }
        activation.ended_at_ms = Some(now);
        activation.termination_reason = update.termination_reason;
        activation.capture_phase = "failed".to_string();
        activation.pipe_acknowledged = false;
        activation.capture_complete = false;
        activation.preservation_status = "failed".to_string();
        let activation = activation.clone();
        candidate
            .preservation_errors
            .push(RunAssetPreservationError {
                activation_id: Some(activation.activation_id.clone()),
                stage: update.stage,
                error: update.error,
                recorded_at_ms: now,
            });
        candidate.preservation_blocked = true;
        candidate.updated_at_ms = now;
        refresh_completion(&mut candidate);
        let preservation_error = candidate
            .preservation_errors
            .last()
            .expect("pushed preservation error should exist")
            .clone();
        records::record_preservation_failure(&mut candidate, &preservation_error)?;
        records::record_activation_probe(
            &mut candidate,
            &activation.activation_id,
            &activation.node_id,
            ActivationProbeState::Suspended,
            now,
        )?;
        let _ = write_activation_metadata_file(&activation);
        let manifest_result = write_manifest_file(&candidate);
        *manifest = candidate;
        manifest_result
    }

    pub fn record_session_association(
        &self,
        manifest: &mut RunAssetManifest,
        session_id: &str,
        relation: SessionRelation,
        activation_id: Option<&str>,
    ) -> Result<(), RunAssetError> {
        let now = self.now_ms();
        records::record_session_relation(manifest, session_id, relation, activation_id, now)?;
        self.write_manifest(manifest)
    }

    pub fn record_hook_fact(
        &self,
        manifest: &mut RunAssetManifest,
        input: HookFactInput,
    ) -> Result<u64, RunAssetError> {
        let now = self.now_ms();
        if let Some(activation_id) = input.activation_id.as_deref() {
            if let Some(activation) = manifest.activations.get(activation_id)
                && !activation.session_id.is_empty()
                && activation.session_id != input.session_id
            {
                return Err(RunAssetError::new(format!(
                    "hook session {} does not execute activation {activation_id}",
                    input.session_id
                )));
            }
            records::record_session_relation(
                manifest,
                &input.session_id,
                SessionRelation::Executes,
                Some(activation_id),
                now,
            )?;
        }
        let generation = records::record_hook_fact(manifest, input, now)?;
        self.write_manifest(manifest)?;
        Ok(generation)
    }

    pub fn record_runtime_event(
        &self,
        manifest: &mut RunAssetManifest,
        event: &crate::runtime::Event,
    ) -> Result<(), RunAssetError> {
        let now = self.now_ms();
        records::record_runtime_event(manifest, event, now)?;
        self.write_manifest(manifest)
    }

    pub fn record_machine_input(
        &self,
        manifest: &mut RunAssetManifest,
        role: &str,
        record: &crate::input_ledger::MachineInputRecord,
    ) -> Result<(), RunAssetError> {
        let now = self.now_ms();
        records::record_machine_input(manifest, role, record, now)?;
        self.write_manifest(manifest)
    }

    pub fn record_qos_intent(
        &self,
        manifest: &mut RunAssetManifest,
        qos: &crate::flow::FlowQosIntent,
    ) -> Result<(), RunAssetError> {
        let now = self.now_ms();
        records::record_qos_intent(manifest, qos, now)?;
        self.write_manifest(manifest)
    }

    pub fn record_topology_decision(
        &self,
        manifest: &mut RunAssetManifest,
        input: TopologyDecisionInput,
    ) -> Result<(), RunAssetError> {
        let now = self.now_ms();
        records::record_topology_decision(manifest, input, now)?;
        self.write_manifest(manifest)
    }

    pub fn rebuild_record_index(
        &self,
        manifest: &RunAssetManifest,
    ) -> Result<serde_json::Value, RunAssetError> {
        records::rebuild_record_index(manifest).and_then(|index| {
            serde_json::to_value(index).map_err(|err| {
                RunAssetError::new(format!("serialize durable record index failed: {err}"))
            })
        })
    }

    pub fn manifest_json(
        &self,
        manifest: &RunAssetManifest,
    ) -> Result<serde_json::Value, RunAssetError> {
        let mut value = serde_json::to_value(manifest).map_err(|err| {
            RunAssetError::new(format!("serialize run asset manifest failed: {err}"))
        })?;
        if let serde_json::Value::Object(object) = &mut value {
            let record_index = records::record_index(manifest).and_then(|index| {
                serde_json::to_value(index).map_err(|err| {
                    RunAssetError::new(format!("serialize durable record index failed: {err}"))
                })
            })?;
            object.insert("records".to_string(), record_index);
        }
        Ok(value)
    }

    fn write_manifest(&self, manifest: &mut RunAssetManifest) -> Result<(), RunAssetError> {
        manifest.updated_at_ms = self.now_ms();
        refresh_completion(manifest);
        write_manifest_file(manifest)
    }

    fn now_ms(&self) -> u64 {
        match self.clock {
            RunAssetClock::Realtime => now_ms(),
            RunAssetClock::Fixed(value) => value,
        }
    }

    #[allow(deprecated)]
    fn selected_sink(&self) -> SelectedRunAssetSink {
        match &self.sink {
            RunAssetSink::Auto => selected_runtime_sink(),
            RunAssetSink::HumanizeRunsDir(path) => {
                SelectedRunAssetSink::HumanizeRunsDir(path.clone())
            }
            RunAssetSink::SforgePatchDir(path) => {
                SelectedRunAssetSink::HumanizeRunsDir(path.clone())
            }
            RunAssetSink::CacheHome(path) => SelectedRunAssetSink::CacheHome(path.clone()),
            RunAssetSink::Root(path) => SelectedRunAssetSink::Root(path.clone()),
        }
    }

    fn fail_if_fault(
        &self,
        point: RunAssetFaultPoint,
        activation_id: &str,
    ) -> Result<(), RunAssetError> {
        self.fail_if_fault_kind(point.into(), activation_id)
    }

    fn fail_if_fault_kind(
        &self,
        point: RunAssetFaultKind,
        activation_id: &str,
    ) -> Result<(), RunAssetError> {
        let Some(fault) = &self.fault else {
            return Ok(());
        };
        if fault.point == point
            && fault
                .activation_id
                .as_deref()
                .map(|expected| expected == activation_id)
                .unwrap_or(true)
            && (!fault.trigger_once || !fault.triggered.replace(true))
        {
            return Err(RunAssetError::new(format!(
                "injected run asset failure at {}",
                point.name()
            )));
        }
        Ok(())
    }
}

fn existing_run_storage_error(run_id: &str, run_root: &Path) -> RunAssetError {
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RunAssetSink {
    Auto,
    HumanizeRunsDir(PathBuf),
    #[deprecated(note = "use HumanizeRunsDir; runtime_default does not inspect SFORGE_PATCH_DIR")]
    SforgePatchDir(PathBuf),
    CacheHome(PathBuf),
    Root(PathBuf),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum RunAssetClock {
    Realtime,
    Fixed(u64),
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum SelectedRunAssetSink {
    HumanizeRunsDir(PathBuf),
    CacheHome(PathBuf),
    Root(PathBuf),
}

impl SelectedRunAssetSink {
    fn name(&self) -> &'static str {
        match self {
            Self::HumanizeRunsDir(_) => "humanize_runs_dir",
            Self::CacheHome(_) => "cache_home",
            Self::Root(_) => "root",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetManifest {
    pub version: u32,
    pub run_id: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub sink: String,
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub storage: RunAssetStorage,
    pub protocol: RunAssetProtocol,
    pub flow: RunAssetFlow,
    pub artifact_paths: RunAssetArtifactPaths,
    pub activations: BTreeMap<String, RunAssetActivation>,
    pub preservation_errors: Vec<RunAssetPreservationError>,
    pub preservation_blocked: bool,
    pub completion: RunAssetCompletion,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetStorage {
    pub raw_run_id: String,
    pub run_directory: String,
    pub run_relative_path: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetProtocol {
    pub mcp_protocol_version: String,
    pub package_name: String,
    pub package_version: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetFlow {
    pub main_flow: bool,
    pub status: String,
    pub complete: bool,
    pub current_revision_id: Option<String>,
    pub current_export_path: Option<PathBuf>,
    pub current_export_relative_path: Option<String>,
    pub revisions: Vec<RunAssetFlowRevision>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetFlowRevision {
    pub revision_id: String,
    pub main_flow: bool,
    pub flow_lock_id: String,
    pub content_hash: String,
    pub review_status: String,
    pub flow_lock_mode: String,
    pub export_format: String,
    pub export_path: PathBuf,
    pub relative_path: String,
    pub created_at_ms: u64,
    pub apply_state: String,
    pub applied_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetArtifactPaths {
    pub manifest: PathBuf,
    pub manifest_relative_path: String,
    pub flow_current: Option<PathBuf>,
    pub flow_current_relative_path: Option<String>,
    pub flow_revisions: Vec<PathBuf>,
    pub flow_revision_relative_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetCompletion {
    pub flow_complete: bool,
    pub expected_tmux_activations: Vec<String>,
    pub complete_tmux_activations: Vec<String>,
    pub incomplete_tmux_activations: Vec<String>,
    pub complete: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetActivation {
    pub run_id: String,
    pub activation_id: String,
    pub node_id: String,
    pub adapter: String,
    pub tmux_target: String,
    pub session_id: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
    pub metadata_path: PathBuf,
    pub pipe_path: PathBuf,
    pub final_capture_path: PathBuf,
    pub relative_paths: RunAssetActivationRelativePaths,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub termination_reason: Option<String>,
    pub capture_phase: String,
    pub pipe_acknowledged: bool,
    pub capture_complete: bool,
    pub preservation_status: String,
    pub resource_cleanup_status: String,
    pub resource_cleanup_error: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetActivationRelativePaths {
    pub metadata: String,
    pub transcript_pipe: String,
    pub final_capture: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetActivationUpdate {
    pub activation_id: String,
    pub node_id: String,
    pub tmux: RunAssetTmuxTarget,
    pub adapter: String,
    pub termination_reason: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetActivationFailureUpdate {
    pub activation_id: String,
    pub node_id: String,
    pub tmux: Option<RunAssetTmuxTarget>,
    pub adapter: String,
    pub termination_reason: Option<String>,
    pub stage: String,
    pub error: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetTmuxTarget {
    pub session_id: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
}

impl RunAssetTmuxTarget {
    pub fn target(&self) -> String {
        format!("{}:{}.{}", self.session_id, self.window_id, self.pane_id)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetActivationPaths {
    pub metadata_path: PathBuf,
    pub pipe_path: PathBuf,
    pub pipe_relative_path: String,
    pub pipe_identity: PipeSinkIdentity,
    pub final_capture_path: PathBuf,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetPreservationError {
    pub activation_id: Option<String>,
    pub stage: String,
    pub error: String,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetError {
    message: String,
}

impl RunAssetError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for RunAssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl Error for RunAssetError {}

fn selected_runtime_sink() -> SelectedRunAssetSink {
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

fn create_dir_all(path: &Path) -> Result<(), RunAssetError> {
    durable_fs::create_dir_all(path)
}

fn write_manifest_file(manifest: &RunAssetManifest) -> Result<(), RunAssetError> {
    if let Some(parent) = manifest.manifest_path.parent() {
        create_dir_all(parent)?;
        ensure_private_dir(parent)?;
    }
    let payload = serde_json::to_string_pretty(manifest)
        .map_err(|err| RunAssetError::new(format!("serialize run asset manifest failed: {err}")))?;
    atomic_write_private(&manifest.manifest_path, payload.as_bytes()).map_err(|err| {
        RunAssetError::new(format!(
            "write run asset manifest {} failed: {err}",
            manifest.manifest_path.display()
        ))
    })
}

fn write_manifest_file_create_new(manifest: &RunAssetManifest) -> Result<(), RunAssetError> {
    if let Some(parent) = manifest.manifest_path.parent() {
        create_dir_all(parent)?;
        ensure_private_dir(parent)?;
    }
    let payload = serde_json::to_string_pretty(manifest)
        .map_err(|err| RunAssetError::new(format!("serialize run asset manifest failed: {err}")))?;
    write_create_new_private(&manifest.manifest_path, payload.as_bytes()).map_err(|err| {
        RunAssetError::new(format!(
            "create run asset manifest {} failed: {err}",
            manifest.manifest_path.display()
        ))
    })
}

fn write_activation_metadata_file(activation: &RunAssetActivation) -> Result<(), RunAssetError> {
    if let Some(parent) = activation.metadata_path.parent() {
        create_dir_all(parent)?;
        ensure_private_dir(parent)?;
    }
    let payload = serde_json::to_string_pretty(activation).map_err(|err| {
        RunAssetError::new(format!("serialize activation metadata failed: {err}"))
    })?;
    atomic_write_private(&activation.metadata_path, payload.as_bytes()).map_err(|err| {
        RunAssetError::new(format!(
            "write activation metadata {} failed: {err}",
            activation.metadata_path.display()
        ))
    })
}

fn ensure_private_dir(path: &Path) -> Result<(), RunAssetError> {
    durable_fs::ensure_private_dir(path)
}

fn open_pipe_log_private(path: &Path) -> Result<PipeSinkIdentity, RunAssetError> {
    durable_fs::open_pipe_log_private(path)
}

fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), RunAssetError> {
    durable_fs::atomic_write_private(path, bytes)
}

pub(crate) fn append_private_line(path: &Path, line: &[u8]) -> Result<(), RunAssetError> {
    durable_fs::append_private_line(path, line)
}

pub(crate) fn append_machine_input_ledger_direct(
    path: &Path,
    record: &crate::input_ledger::MachineInputRecord,
) -> Result<(), RunAssetError> {
    records::append_machine_input_ledger_direct(path, record)
}

pub(crate) fn read_regular_private(path: &Path) -> Result<Option<Vec<u8>>, RunAssetError> {
    durable_fs::read_regular_private(path)
}

fn write_create_new_private(path: &Path, bytes: &[u8]) -> Result<(), RunAssetError> {
    durable_fs::write_create_new_private(path, bytes)
}

fn refresh_completion(manifest: &mut RunAssetManifest) {
    let mut expected = Vec::new();
    let mut complete = Vec::new();
    let mut incomplete = Vec::new();
    for (activation_id, activation) in &manifest.activations {
        expected.push(activation_id.clone());
        if activation_complete(activation) {
            complete.push(activation_id.clone());
        } else {
            incomplete.push(activation_id.clone());
        }
    }
    manifest.completion = RunAssetCompletion {
        flow_complete: manifest.flow.complete,
        expected_tmux_activations: expected,
        complete_tmux_activations: complete,
        incomplete_tmux_activations: incomplete.clone(),
        complete: manifest.flow.complete
            && incomplete.is_empty()
            && manifest.preservation_errors.is_empty()
            && !manifest.preservation_blocked,
    };
}

fn activation_complete(activation: &RunAssetActivation) -> bool {
    activation.pipe_acknowledged
        && activation.capture_phase == "complete"
        && activation.capture_complete
        && activation.ended_at_ms.is_some()
        && activation.termination_reason.is_some()
        && activation.preservation_status == "complete"
        && activation.final_capture_path.exists()
}

fn activation_can_complete(activation: &RunAssetActivation) -> bool {
    activation.pipe_acknowledged
        && activation.capture_phase == "capturing"
        && activation.preservation_status == "capturing"
}

fn relative_path_string(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn flow_check_mode_name(mode: FlowCheckMode) -> &'static str {
    match mode {
        FlowCheckMode::Core => "core",
        FlowCheckMode::Strict => "strict",
    }
}

fn storage_segment(domain: &str, raw_id: &str) -> String {
    let slug = raw_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('.')
        .to_string();
    let mut slug = if slug.is_empty() || slug == "." || slug == ".." {
        "id".to_string()
    } else {
        slug
    };
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(raw_id.as_bytes());
    let digest = hasher.finalize();
    let hash = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let prefix = format!("{domain}-sha256-{hash}-");
    let slug_limit = STORAGE_SEGMENT_MAX_BYTES.saturating_sub(prefix.len());
    slug.truncate(slug_limit);
    format!("{prefix}{slug}")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
