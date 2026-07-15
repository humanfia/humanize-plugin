use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::flow::{self, FlowCheckMode, FlowExportFormat, FlowLock};
use crate::pipe_sink::PipeSinkIdentity;

mod authority;
pub(crate) mod durable_fs;
mod fault;
mod journal;
mod model;
mod ownership;
mod public_event;
mod public_metadata;
pub(crate) mod publication;
mod readiness;
mod records;
mod store_files;

pub(crate) use authority::read_manifest_for_run_root;
use authority::validate_manifest_layout;
use fault::RunAssetFaultKind;
pub use fault::{RunAssetFault, RunAssetFaultPoint};
pub use model::{
    RunAssetActivation, RunAssetActivationFailureUpdate, RunAssetActivationRelativePaths,
    RunAssetActivationUpdate, RunAssetArtifactPaths, RunAssetCompletion, RunAssetError,
    RunAssetFlow, RunAssetFlowRevision, RunAssetManifest, RunAssetPreservationError,
    RunAssetProtocol, RunAssetSink, RunAssetStorage,
};
use model::{RunAssetClock, SelectedRunAssetSink};
pub use ownership::{
    OwnedTmuxPane, RunAssetActivationPaths, RunAssetTmuxTarget,
    discover_live_owned_tmux_panes_in_dir, discover_private_owned_tmux_panes_in_dir,
};
pub(crate) use public_event::*;
use public_metadata::{public_hash_ref, write_activation_metadata_file};
use readiness::random_private_nonce;
pub use records::{
    ActivationProbeState, HookFactDetail, HookFactInput, RunAssetRecordFile, RunAssetRecordIndex,
    RunAssetSessionIndex, RunAssetSessionRelation, SessionRelation, TopologyDecisionInput,
    TopologyDecisionSource,
};
pub(crate) use records::{
    PublicRecordBatch, machine_input_source_native_id, session_source_native_id,
};
use store_files::*;

pub const RUN_ASSET_PROTOCOL_VERSION: &str = "2024-11-05";
pub const RUN_ASSET_PACKAGE_NAME: &str = "humanize-plugin";
pub const AGENT_READY_HOOK: &str = "humanize.agent_ready";
pub const AGENT_READY_FAILURE_HOOK: &str = "humanize.agent_ready_failure";
pub const TMUX_GUARD_BLOCKED_HOOK: &str = "humanize.tmux_guard_blocked";
const STORAGE_SEGMENT_MAX_BYTES: usize = 180;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxGuardBlockedEvidence {
    pub operation: String,
    pub option_flags: Vec<String>,
    pub target_hash: String,
    pub payload_length: u64,
    pub payload_hash: String,
    pub evidence_hash: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetStore {
    sink: RunAssetSink,
    clock: RunAssetClock,
    fault: Option<RunAssetFault>,
    private_runtime_root: Option<PathBuf>,
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
            private_runtime_root: None,
        }
    }

    pub fn new_with_fixed_clock(sink: RunAssetSink, timestamp_ms: u64) -> Self {
        Self {
            sink,
            clock: RunAssetClock::Fixed(timestamp_ms),
            fault: None,
            private_runtime_root: None,
        }
    }

    #[doc(hidden)]
    pub fn new_with_fault(sink: RunAssetSink, fault: RunAssetFault) -> Self {
        Self {
            sink,
            clock: RunAssetClock::Realtime,
            fault: Some(fault),
            private_runtime_root: None,
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
            private_runtime_root: None,
        }
    }

    pub(crate) fn new_driver_owned(sink: RunAssetSink, private_runtime_root: PathBuf) -> Self {
        Self {
            sink,
            clock: RunAssetClock::Realtime,
            fault: None,
            private_runtime_root: Some(private_runtime_root),
        }
    }

    pub fn run_root(&self, run_id: &str) -> Result<PathBuf, RunAssetError> {
        self.validate_runtime_override()?;
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

    pub fn runs_root(&self) -> Result<PathBuf, RunAssetError> {
        self.validate_runtime_override()?;
        match self.selected_sink() {
            SelectedRunAssetSink::HumanizeRunsDir(path) | SelectedRunAssetSink::Root(path) => {
                Ok(path)
            }
            SelectedRunAssetSink::CacheHome(path) => {
                Ok(path.join(".cache").join("humanize").join("runs"))
            }
        }
    }

    pub fn discover_live_owned_tmux_panes(&self) -> Result<Vec<OwnedTmuxPane>, RunAssetError> {
        discover_live_owned_tmux_panes_in_dir(&self.runs_root()?)
    }

    pub fn discover_private_owned_tmux_panes(&self) -> Result<Vec<OwnedTmuxPane>, RunAssetError> {
        discover_private_owned_tmux_panes_in_dir(&self.runs_root()?)
    }

    pub fn start_run_manifest(&self, run_id: &str) -> Result<RunAssetManifest, RunAssetError> {
        self.create_run_manifest(run_id, true)
    }

    pub fn load_or_start_run_manifest(
        &self,
        run_id: &str,
    ) -> Result<RunAssetManifest, RunAssetError> {
        let run_root = self.run_root(run_id)?;
        let manifest_path = self
            .private_manifest_path(&run_root)
            .unwrap_or_else(|| run_root.join("manifest.json"));
        match fs::symlink_metadata(&manifest_path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                let manifest = self.load_manifest(run_id)?;
                return Ok(manifest);
            }
            Ok(_) => {
                return Err(RunAssetError::new(format!(
                    "run asset manifest {} is not a regular file",
                    manifest_path.display()
                )));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(RunAssetError::new(format!(
                    "inspect run asset manifest {} failed: {err}",
                    manifest_path.display()
                )));
            }
        }
        self.create_run_manifest(run_id, false)
    }

    fn create_run_manifest(
        &self,
        run_id: &str,
        require_absent_root: bool,
    ) -> Result<RunAssetManifest, RunAssetError> {
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
            Ok(_) if require_absent_root => {
                return Err(existing_run_storage_error(run_id, &run_root));
            }
            Ok(_) => {}
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
        self.write_manifest_create_new(&manifest)?;
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
        self.validate_manifest_authority(manifest)?;
        let revision_id = format!("rev-{:04}", manifest.flow.revisions.len() + 1);
        let revision_path = manifest
            .root
            .join("flow")
            .join("revisions")
            .join(&revision_id)
            .join("flow-lock.json");
        let exported = flow::flow_export(lock, FlowExportFormat::Json);
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
        let mut publication = PublicRecordBatch::default();
        records::record_flow_revision(
            &mut publication,
            &mut candidate,
            &revision,
            "prepared",
            now,
            Some(exported.as_bytes()),
        )?;
        self.write_manifest_with_publication(&mut candidate, &publication)?;
        *manifest = candidate;
        Ok(revision)
    }

    pub fn commit_flow_revision_applied(
        &self,
        manifest: &mut RunAssetManifest,
        revision_id: &str,
    ) -> Result<RunAssetFlowRevision, RunAssetError> {
        self.validate_manifest_authority(manifest)?;
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
        let mut publication = PublicRecordBatch::default();
        records::record_flow_revision(
            &mut publication,
            &mut candidate,
            &revision,
            "applied",
            now,
            None,
        )?;
        self.write_manifest_with_publication(&mut candidate, &publication)?;
        *manifest = candidate;
        Ok(revision)
    }

    pub fn mark_flow_revision_failed(
        &self,
        manifest: &mut RunAssetManifest,
        revision_id: &str,
        error: &str,
    ) -> Result<(), RunAssetError> {
        self.validate_manifest_authority(manifest)?;
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
        let mut publication = PublicRecordBatch::default();
        records::record_preservation_failure(
            &mut publication,
            &mut candidate,
            &preservation_error,
        )?;
        if let Some(revision) = candidate
            .flow
            .revisions
            .iter()
            .find(|revision| revision.revision_id == revision_id)
            .cloned()
        {
            records::record_flow_revision(
                &mut publication,
                &mut candidate,
                &revision,
                "failed",
                now,
                None,
            )?;
        }
        self.write_manifest_with_publication(&mut candidate, &publication)?;
        *manifest = candidate;
        Ok(())
    }

    pub fn start_activation_capture(
        &self,
        manifest: &mut RunAssetManifest,
        update: RunAssetActivationUpdate,
    ) -> Result<RunAssetActivationPaths, RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        self.fail_if_fault(
            RunAssetFaultPoint::StartActivationTranscript,
            &update.activation_id,
        )?;
        let capture_root = self.activation_capture_root(manifest);
        let activation_root = capture_root
            .join("activations")
            .join(storage_segment("act", &update.activation_id));
        let activation_dir =
            allocation_capture_dir(&activation_root, update.tmux.allocation_generation);
        create_dir_all(&activation_dir)?;
        ensure_private_dir(&activation_dir)?;
        let metadata_path = activation_dir.join("metadata.json");
        let pipe_path = activation_dir.join("transcript.pipe.log");
        let final_capture_path = activation_dir.join("final-capture.txt");
        let pipe_identity = open_pipe_log_private(&pipe_path)?;
        let pipe_relative_path = relative_path_string(&capture_root, &pipe_path);
        let readiness_nonce = manifest
            .activations
            .get(&update.activation_id)
            .filter(|activation| {
                activation.pane_id == update.tmux.pane_id
                    && activation.allocation_generation == update.tmux.allocation_generation
                    && !activation.readiness_nonce.is_empty()
            })
            .map(|activation| activation.readiness_nonce.clone())
            .map(Ok)
            .unwrap_or_else(random_private_nonce)?;

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
            tmux_target_ref: String::new(),
            session_ref: String::new(),
            window_ref: String::new(),
            window_name_ref: String::new(),
            pane_ref: String::new(),
            allocation_generation: update.tmux.allocation_generation,
            readiness_nonce,
            metadata_path: metadata_path.clone(),
            pipe_path: pipe_path.clone(),
            final_capture_path: final_capture_path.clone(),
            relative_paths: RunAssetActivationRelativePaths {
                metadata: relative_path_string(&capture_root, &metadata_path),
                transcript_pipe: relative_path_string(&capture_root, &pipe_path),
                final_capture: relative_path_string(&capture_root, &final_capture_path),
            },
            started_at_ms: now,
            ended_at_ms: None,
            termination_reason: update.termination_reason,
            capture_phase: "starting".to_string(),
            pipe_acknowledged: false,
            capture_complete: false,
            preservation_status: "starting".to_string(),
            resource_cleanup_status: "owned".to_string(),
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
        let mut publication = PublicRecordBatch::default();
        records::record_tmux_activation(
            &mut publication,
            &mut candidate,
            &activation,
            "capture_started",
            now,
        )?;
        records::record_activation_probe(
            &mut publication,
            &mut candidate,
            &activation.activation_id,
            &activation.node_id,
            ActivationProbeState::Ready,
            now,
        )?;
        self.fail_if_fault(RunAssetFaultPoint::StartActivationManifest, &activation_id)?;
        self.write_manifest_with_publication(&mut candidate, &publication)?;
        *manifest = candidate;

        Ok(RunAssetActivationPaths {
            capture_root,
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
        self.validate_manifest_authority(manifest)?;
        if manifest.activations.contains_key(activation_id) {
            return Ok(());
        }
        self.fail_if_fault(
            RunAssetFaultPoint::RegisterExpectedActivation,
            activation_id,
        )?;
        let capture_root = self.activation_capture_root(manifest);
        let activation_dir = capture_root
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
            tmux_target_ref: String::new(),
            session_ref: String::new(),
            window_ref: String::new(),
            window_name_ref: String::new(),
            pane_ref: String::new(),
            allocation_generation: 0,
            readiness_nonce: String::new(),
            metadata_path: metadata_path.clone(),
            pipe_path: pipe_path.clone(),
            final_capture_path: final_capture_path.clone(),
            relative_paths: RunAssetActivationRelativePaths {
                metadata: relative_path_string(&capture_root, &metadata_path),
                transcript_pipe: relative_path_string(&capture_root, &pipe_path),
                final_capture: relative_path_string(&capture_root, &final_capture_path),
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
        let mut publication = PublicRecordBatch::default();
        records::record_activation_probe(
            &mut publication,
            &mut candidate,
            activation_id,
            node_id,
            ActivationProbeState::Planned,
            now,
        )?;
        self.write_manifest_with_publication(&mut candidate, &publication)?;
        *manifest = candidate;
        Ok(())
    }

    pub fn mark_activation_capture_acknowledged(
        &self,
        manifest: &mut RunAssetManifest,
        activation_id: &str,
    ) -> Result<RunAssetActivation, RunAssetError> {
        self.validate_manifest_authority(manifest)?;
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
        let now = records::recorded_tmux_activation_time(
            manifest,
            activation_id,
            existing.allocation_generation,
            "capture_completed",
        )?
        .unwrap_or_else(|| self.now_ms());
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
        let mut publication = PublicRecordBatch::default();
        records::record_tmux_activation(
            &mut publication,
            &mut candidate,
            &activation,
            "capture_acknowledged",
            now,
        )?;
        self.write_manifest_with_publication(&mut candidate, &publication)?;
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
        self.validate_manifest_authority(manifest)?;
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

        let recorded_at = records::recorded_tmux_activation_time(
            manifest,
            activation_id,
            existing.allocation_generation,
            "capture_completed",
        )?;
        let now = recorded_at.unwrap_or_else(|| self.now_ms());
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
        let mut publication = PublicRecordBatch::default();
        if recorded_at.is_none() {
            records::record_tmux_activation(
                &mut publication,
                &mut candidate,
                &activation,
                "capture_completed",
                now,
            )?;
        }
        records::record_activation_probe(
            &mut publication,
            &mut candidate,
            &activation.activation_id,
            &activation.node_id,
            ActivationProbeState::Closed,
            now,
        )?;
        self.write_manifest_with_publication(&mut candidate, &publication)?;
        *manifest = candidate;
        Ok(activation)
    }

    pub fn persist_activation_final_capture_snapshot(
        &self,
        manifest: &RunAssetManifest,
        activation_id: &str,
        final_capture: &str,
    ) -> Result<PathBuf, RunAssetError> {
        self.validate_manifest_authority(manifest)?;
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
        self.validate_manifest_authority(manifest)?;
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

        let recorded_at = records::recorded_tmux_activation_time(
            manifest,
            activation_id,
            existing.allocation_generation,
            "capture_failed",
        )?;
        let now = recorded_at.unwrap_or_else(|| self.now_ms());
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
        let mut publication = PublicRecordBatch::default();
        if recorded_at.is_none() {
            records::record_tmux_activation(
                &mut publication,
                &mut candidate,
                &activation,
                "capture_failed",
                now,
            )?;
        }
        records::record_activation_probe(
            &mut publication,
            &mut candidate,
            &activation.activation_id,
            &activation.node_id,
            ActivationProbeState::Suspended,
            now,
        )?;
        self.write_manifest_with_publication(&mut candidate, &publication)?;
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
        self.validate_manifest_authority(manifest)?;
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
        let mut publication = PublicRecordBatch::default();
        records::record_tmux_activation(
            &mut publication,
            &mut candidate,
            &activation,
            &format!("resource_cleanup_{status}"),
            now,
        )?;
        self.write_manifest_with_publication(&mut candidate, &publication)?;
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
        self.validate_manifest_authority(manifest)?;
        let now = self.now_ms();
        manifest.updated_at_ms = now;
        let mut updated_activation = None;
        if let Some(activation_id) = activation_id
            && let Some(activation) = manifest.activations.get_mut(activation_id)
        {
            activation.ended_at_ms = Some(now);
            if let Some(reason) = termination_reason {
                activation.termination_reason = Some(reason.to_string());
            }
            activation.capture_phase = "failed".to_string();
            activation.capture_complete = false;
            activation.preservation_status = "failed".to_string();
            updated_activation = Some(activation.clone());
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
        let mut publication = PublicRecordBatch::default();
        records::record_preservation_failure(&mut publication, manifest, &preservation_error)?;
        if let Some(activation_id) = activation_id
            && let Some(activation) = manifest.activations.get(activation_id).cloned()
        {
            records::record_activation_probe(
                &mut publication,
                manifest,
                &activation.activation_id,
                &activation.node_id,
                ActivationProbeState::Suspended,
                now,
            )?;
        }
        refresh_completion(manifest);
        let manifest_result = self.write_manifest_with_publication(manifest, &publication);
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
        self.validate_manifest_authority(manifest)?;
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
                tmux_target_ref: String::new(),
                session_ref: String::new(),
                window_ref: String::new(),
                window_name_ref: String::new(),
                pane_ref: String::new(),
                allocation_generation: update
                    .tmux
                    .as_ref()
                    .map(|target| target.allocation_generation)
                    .unwrap_or(0),
                readiness_nonce: String::new(),
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
            let tmux_target = tmux.target();
            let window_target = format!("{}:{}", tmux.session_id, tmux.window_id);
            activation.tmux_target_ref = public_hash_ref(&tmux_target);
            activation.session_ref = public_hash_ref(&tmux.session_id);
            activation.window_ref = public_hash_ref(&window_target);
            activation.window_name_ref = public_hash_ref(&tmux.window_name);
            activation.pane_ref = public_hash_ref(&tmux.pane_id);
            activation.tmux_target = tmux_target;
            activation.session_id = tmux.session_id;
            activation.window_id = tmux.window_id;
            activation.window_name = tmux.window_name;
            activation.pane_id = tmux.pane_id;
            activation.allocation_generation = tmux.allocation_generation;
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
        let mut publication = PublicRecordBatch::default();
        records::record_preservation_failure(
            &mut publication,
            &mut candidate,
            &preservation_error,
        )?;
        records::record_activation_probe(
            &mut publication,
            &mut candidate,
            &activation.activation_id,
            &activation.node_id,
            ActivationProbeState::Suspended,
            now,
        )?;
        let _ = write_activation_metadata_file(&activation);
        let manifest_result = self.write_manifest_with_publication(&mut candidate, &publication);
        *manifest = candidate;
        manifest_result
    }

    pub fn record_session_association(
        &self,
        manifest: &mut RunAssetManifest,
        session_id: &str,
        relation: SessionRelation,
        activation_id: Option<&str>,
        platform: &str,
        exit_status: Option<i32>,
    ) -> Result<(), RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        let now = self.now_ms();
        let mut publication = PublicRecordBatch::default();
        records::record_session_relation(
            &mut publication,
            manifest,
            records::SessionFactInput {
                session_id,
                relation,
                activation_id,
                platform,
                exit_status,
                now_ms: now,
            },
        )?;
        self.write_manifest_with_publication(manifest, &publication)
    }

    pub fn record_hook_fact(
        &self,
        manifest: &mut RunAssetManifest,
        input: HookFactInput,
    ) -> Result<u64, RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        let now = self.now_ms();
        if let Some(activation_id) = input.activation_id.as_deref()
            && let Some(activation) = manifest.activations.get(activation_id)
            && !activation.session_id.is_empty()
            && activation.session_id != input.session_id
        {
            return Err(RunAssetError::new(format!(
                "hook session {} does not execute activation {activation_id}",
                input.session_id
            )));
        }
        let mut publication = PublicRecordBatch::default();
        let generation = records::record_hook_fact(&mut publication, manifest, input, now)?;
        self.write_manifest_with_publication(manifest, &publication)?;
        Ok(generation)
    }

    pub fn record_runtime_event(
        &self,
        manifest: &mut RunAssetManifest,
        event: &crate::runtime::Event,
    ) -> Result<bool, RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        let now = self.now_ms();
        let mut publication = PublicRecordBatch::default();
        let appended = records::record_runtime_event(&mut publication, manifest, event, now)?;
        if appended {
            self.write_manifest_with_publication(manifest, &publication)?;
        }
        Ok(appended)
    }

    pub(crate) fn prepare_runtime_publication(
        &self,
        manifest: &RunAssetManifest,
        events: &[crate::runtime::Event],
        routes: &[crate::runtime::RouteDecision],
    ) -> Result<PublicRecordBatch, RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        records::prepare_runtime_publication(manifest, events, routes, self.now_ms())
    }

    pub(crate) fn reconcile_public_seal(
        &self,
        manifest: &RunAssetManifest,
        private_terminal: bool,
        mutation: bool,
    ) -> Result<(), RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        journal::reconcile_public_seal(manifest, private_terminal, mutation)
    }

    pub(crate) fn repair_public_manifest_projection(
        &self,
        manifest: &RunAssetManifest,
    ) -> Result<(), RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        self.write_public_manifest_projection(manifest)
    }

    pub fn record_machine_input(
        &self,
        manifest: &mut RunAssetManifest,
        role: &str,
        record: &crate::input_ledger::MachineInputRecord,
    ) -> Result<(), RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        let activation = manifest
            .activations
            .get(&record.activation_id)
            .ok_or_else(|| {
                RunAssetError::new(format!(
                    "activation not found for machine input: {}",
                    record.activation_id
                ))
            })?;
        if (!activation.pane_id.is_empty() && activation.pane_id != record.pane_id)
            || activation.allocation_generation != record.allocation_generation
        {
            return Err(RunAssetError::new(
                "machine input does not match the current pane allocation",
            ));
        }
        let now = self.now_ms();
        let mut publication = PublicRecordBatch::default();
        records::record_machine_input(&mut publication, manifest, role, record, now)?;
        self.write_manifest_with_publication(manifest, &publication)
    }

    pub fn record_qos_intent(
        &self,
        manifest: &mut RunAssetManifest,
        qos: &crate::flow::FlowQosIntent,
    ) -> Result<(), RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        let now = self.now_ms();
        let mut publication = PublicRecordBatch::default();
        records::record_qos_intent(&mut publication, manifest, qos, now)?;
        self.write_manifest_with_publication(manifest, &publication)
    }

    pub fn record_topology_decision(
        &self,
        manifest: &mut RunAssetManifest,
        input: TopologyDecisionInput,
    ) -> Result<(), RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        let now = self.now_ms();
        let mut publication = PublicRecordBatch::default();
        records::record_topology_decision(&mut publication, manifest, input, now)?;
        self.write_manifest_with_publication(manifest, &publication)
    }

    pub fn rebuild_record_index(
        &self,
        manifest: &RunAssetManifest,
    ) -> Result<serde_json::Value, RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        let index = records::rebuild_record_index(manifest)?;
        self.write_public_manifest_projection(manifest)?;
        serde_json::to_value(index).map_err(|err| {
            RunAssetError::new(format!("serialize durable record index failed: {err}"))
        })
    }

    pub fn manifest_json(
        &self,
        manifest: &RunAssetManifest,
    ) -> Result<serde_json::Value, RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        if self.private_runtime_root.is_some() {
            return journal::public_manifest_projection(manifest);
        }
        let mut value = manifest_disk_value(manifest)?;
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

    pub fn load_manifest(&self, run_id: &str) -> Result<RunAssetManifest, RunAssetError> {
        let run_root = self.run_root(run_id)?;
        let manifest = if let Some(path) = self.private_manifest_path(&run_root) {
            self.recover_private_manifest_publications(&run_root)?;
            let bytes = read_regular_private(&path)?.ok_or_else(|| {
                RunAssetError::new(format!(
                    "private run asset manifest {} does not exist",
                    path.display()
                ))
            })?;
            let manifest = serde_json::from_slice::<RunAssetManifest>(&bytes).map_err(|err| {
                RunAssetError::new(format!(
                    "parse private run asset manifest {} failed: {err}",
                    path.display()
                ))
            })?;
            validate_manifest_layout(
                &manifest,
                &run_root,
                &self.activation_capture_root_for_run_root(&run_root),
                Some(run_id),
            )?;
            manifest
        } else {
            read_manifest_for_run_root(&run_root, Some(run_id))?
        };
        if manifest.sink != self.selected_sink().name() {
            return Err(RunAssetError::new(
                "run asset manifest sink does not match the configured store",
            ));
        }
        Ok(manifest)
    }

    fn write_manifest_with_publication(
        &self,
        manifest: &mut RunAssetManifest,
        batch: &PublicRecordBatch,
    ) -> Result<(), RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        manifest.updated_at_ms = self.now_ms();
        refresh_completion(manifest);
        if let Some(path) = self.private_manifest_path(&manifest.root) {
            let private_run_root = self.private_run_root_for_run_root(&manifest.root)?;
            if !publication::pending_transactions(&private_run_root)?.is_empty() {
                return Err(RunAssetError::publication(
                    "pending public publication must reconcile before mutation",
                ));
            }
            if batch.is_empty() {
                write_private_manifest_file(&path, manifest, false)?;
                return self.write_public_manifest_projection(manifest);
            }
            records::preflight_record_batch(manifest, batch)
                .map_err(RunAssetError::publication_from)?;
            let base = read_regular_private(&path)?
                .ok_or_else(|| {
                    RunAssetError::new(format!(
                        "private run asset manifest {} does not exist",
                        path.display()
                    ))
                })
                .and_then(|bytes| {
                    serde_json::from_slice::<RunAssetManifest>(&bytes).map_err(|err| {
                        RunAssetError::new(format!(
                            "parse private run asset manifest {} failed: {err}",
                            path.display()
                        ))
                    })
                })?;
            let transaction = publication::PublicationTransaction::run_asset_manifest(
                Some(publication::manifest_sha256(&base)?),
                manifest.clone(),
                batch.clone(),
            )?;
            let transaction = publication::persist_pending(&private_run_root, transaction)
                .map_err(RunAssetError::publication_from)?;
            self.reconcile_private_manifest_transaction(&transaction)
                .map_err(RunAssetError::publication_from)?;
            self.publish_transaction(&transaction)
                .map_err(RunAssetError::publication_from)?;
            publication::acknowledge(&private_run_root, &transaction)
                .map_err(RunAssetError::publication_from)?;
            return Ok(());
        }
        records::preflight_record_batch(manifest, batch)?;
        records::publish_record_batch(manifest, batch)?;
        self.write_public_manifest_projection(manifest)
    }

    fn write_manifest_create_new(&self, manifest: &RunAssetManifest) -> Result<(), RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        if let Some(path) = self.private_manifest_path(&manifest.root) {
            write_private_manifest_file(&path, manifest, true)?;
            return self.write_public_manifest_projection(manifest);
        }
        write_manifest_file_create_new(manifest)
    }

    fn write_public_manifest_projection(
        &self,
        manifest: &RunAssetManifest,
    ) -> Result<(), RunAssetError> {
        if self.private_runtime_root.is_some() {
            return write_public_projection_file(manifest);
        }
        write_manifest_file(manifest)
    }

    fn private_manifest_path(&self, run_root: &Path) -> Option<PathBuf> {
        self.private_runtime_root.as_ref().map(|runtime_root| {
            crate::state_path::private_run_root(runtime_root, run_root)
                .join("driver")
                .join("run-assets.json")
        })
    }

    pub(crate) fn private_run_root_for_run_root(
        &self,
        run_root: &Path,
    ) -> Result<PathBuf, RunAssetError> {
        self.private_runtime_root
            .as_ref()
            .map(|runtime_root| crate::state_path::private_run_root(runtime_root, run_root))
            .ok_or_else(|| RunAssetError::new("run asset store is not driver-owned"))
    }

    pub(crate) fn reconcile_private_manifest_transaction(
        &self,
        transaction: &publication::PublicationTransaction,
    ) -> Result<(), RunAssetError> {
        let publication::PublicationMutation::RunAssetManifest {
            base_manifest_sha256,
            manifest,
        } = transaction.mutation()
        else {
            return Ok(());
        };
        self.validate_manifest_authority(manifest)?;
        let path = self.private_manifest_path(&manifest.root).ok_or_else(|| {
            RunAssetError::new("run asset publication requires a driver-owned store")
        })?;
        let current = read_regular_private(&path)?;
        let candidate_sha256 = publication::manifest_sha256(manifest)?;
        let current_sha256 = current
            .as_deref()
            .map(|bytes| {
                serde_json::from_slice::<RunAssetManifest>(bytes)
                    .map_err(|err| {
                        RunAssetError::new(format!(
                            "parse private run asset manifest {} failed: {err}",
                            path.display()
                        ))
                    })
                    .and_then(|current| publication::manifest_sha256(&current))
            })
            .transpose()?;
        if current_sha256.as_ref() == Some(&candidate_sha256) {
            return Ok(());
        }
        if &current_sha256 != base_manifest_sha256 {
            return Err(RunAssetError::new(
                "private run asset manifest conflicts with publication outbox",
            ));
        }
        write_private_manifest_file(&path, manifest, current.is_none())
    }

    pub(crate) fn publish_transaction(
        &self,
        transaction: &publication::PublicationTransaction,
    ) -> Result<(), RunAssetError> {
        let manifest = match transaction.mutation() {
            publication::PublicationMutation::RunAssetManifest { manifest, .. } => manifest,
            publication::PublicationMutation::RuntimeEvents { .. } => {
                return Err(RunAssetError::new(
                    "runtime publication requires the current run asset manifest",
                ));
            }
        };
        self.publish_record_batch_and_projection(manifest, transaction.public_records())
    }

    pub(crate) fn publish_record_batch_and_projection(
        &self,
        manifest: &RunAssetManifest,
        batch: &PublicRecordBatch,
    ) -> Result<(), RunAssetError> {
        publication::fail_public_event_if_requested()?;
        records::publish_record_batch(manifest, batch).map_err(RunAssetError::publication_from)?;
        if let Some(path) = env::var_os("HUMANIZE_DRIVER_FAIL_MANIFEST_IF_EXISTS")
            && PathBuf::from(path).exists()
        {
            return Err(RunAssetError::publication(
                "injected public manifest projection failure",
            ));
        }
        self.write_public_manifest_projection(manifest)
            .map_err(RunAssetError::publication_from)
    }

    pub(crate) fn preflight_publication(
        &self,
        manifest: &RunAssetManifest,
        batch: &PublicRecordBatch,
    ) -> Result<(), RunAssetError> {
        records::preflight_record_batch(manifest, batch)
    }

    fn recover_private_manifest_publications(&self, run_root: &Path) -> Result<(), RunAssetError> {
        let private_run_root = self.private_run_root_for_run_root(run_root)?;
        for transaction in publication::pending_transactions(&private_run_root)? {
            self.reconcile_private_manifest_transaction(&transaction)?;
        }
        Ok(())
    }

    pub(crate) fn activation_capture_root(&self, manifest: &RunAssetManifest) -> PathBuf {
        self.activation_capture_root_for_run_root(&manifest.root)
    }

    fn activation_capture_root_for_run_root(&self, run_root: &Path) -> PathBuf {
        self.private_runtime_root.as_ref().map_or_else(
            || run_root.to_path_buf(),
            |runtime_root| {
                crate::state_path::private_run_root(runtime_root, run_root).join("captures")
            },
        )
    }

    fn validate_manifest_authority(
        &self,
        manifest: &RunAssetManifest,
    ) -> Result<(), RunAssetError> {
        let expected_root = self.run_root(&manifest.run_id)?;
        validate_manifest_layout(
            manifest,
            &expected_root,
            &self.activation_capture_root_for_run_root(&expected_root),
            Some(&manifest.run_id),
        )?;
        if manifest.sink != self.selected_sink().name() {
            return Err(RunAssetError::new(
                "run asset manifest sink does not match the configured store",
            ));
        }
        Ok(())
    }

    fn now_ms(&self) -> u64 {
        match self.clock {
            RunAssetClock::Realtime => now_ms(),
            RunAssetClock::Fixed(value) => value,
        }
    }

    fn validate_runtime_override(&self) -> Result<(), RunAssetError> {
        if !matches!(self.sink, RunAssetSink::Auto) {
            return Ok(());
        }
        if let Some(path) = env::var_os("HUMANIZE_RUNS_DIR").filter(|value| !value.is_empty()) {
            return crate::state_path::validate_explicit_state_path(
                "HUMANIZE_RUNS_DIR",
                PathBuf::from(path),
            )
            .map(|_| ())
            .map_err(|error| RunAssetError::new(error.to_string()));
        }
        let home = env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| RunAssetError::new("HOME is required for the run asset store"))?;
        crate::state_path::validate_explicit_state_path("HOME", PathBuf::from(home))
            .map(|_| ())
            .map_err(|error| RunAssetError::new(error.to_string()))
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

fn create_dir_all(path: &Path) -> Result<(), RunAssetError> {
    durable_fs::create_dir_all(path)
}

fn ensure_private_dir(path: &Path) -> Result<(), RunAssetError> {
    durable_fs::ensure_private_dir(path)
}

pub(crate) fn ensure_private_directory(path: &Path) -> Result<(), RunAssetError> {
    durable_fs::create_dir_all(path)?;
    durable_fs::ensure_private_dir(path)
}

pub(crate) fn open_private_lock_file(path: &Path) -> Result<std::fs::File, RunAssetError> {
    durable_fs::open_private_lock_file(path)
}

pub(crate) type PrivateDirectoryFiles = Vec<(std::ffi::OsString, Vec<u8>)>;

pub(crate) fn read_private_directory(
    path: &Path,
) -> Result<Option<PrivateDirectoryFiles>, RunAssetError> {
    durable_fs::read_private_directory(path)
}

pub(crate) fn remove_regular_private(path: &Path) -> Result<(), RunAssetError> {
    durable_fs::remove_regular_private(path)
}

fn open_pipe_log_private(path: &Path) -> Result<PipeSinkIdentity, RunAssetError> {
    durable_fs::open_pipe_log_private(path)
}

pub(crate) fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), RunAssetError> {
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

pub(crate) fn truncate_private(path: &Path, len: u64) -> Result<(), RunAssetError> {
    durable_fs::truncate_private(path, len)
}

pub(crate) fn write_create_new_private(path: &Path, bytes: &[u8]) -> Result<(), RunAssetError> {
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
        && activation.resource_cleanup_status == "complete"
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

fn allocation_capture_dir(activation_root: &Path, allocation_generation: u64) -> PathBuf {
    if allocation_generation == 0 {
        activation_root.to_path_buf()
    } else {
        activation_root.join(format!("allocation-{allocation_generation}"))
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
