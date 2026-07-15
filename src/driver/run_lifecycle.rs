use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::adapters::tmux::{
    PipeCaptureRequest, TmuxActivationMetadata, TmuxPane, TmuxPanePresence, new_pipe_capture_nonce,
};
use crate::pipe_sink::{PipeSinkIdentity, remove_pipe_sink_ack_under_root};
use crate::run_assets::{RunAssetActivationUpdate, RunAssetTmuxTarget, read_regular_private};
use crate::runtime::{self, ControlCommand, DriverTickInput};

use super::flow_lock::StoredFlowRevision;
use super::{
    DriverFailure, DriverPaneConfig, RuntimeDriverService, StoredPane,
    maybe_crash_after_tmux_effect,
};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct TmuxPipeCaptureIntent {
    pub(super) operation_id: String,
    pub(super) activation_id: String,
    pub(super) allocation_generation: u64,
    pub(super) pane: StoredPane,
    pub(super) transcript_relative_path: PathBuf,
    pub(super) transcript_identity: PipeSinkIdentity,
    pub(super) ack_relative_path: PathBuf,
    pub(super) completion_relative_path: PathBuf,
    pub(super) ack_nonce: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct TmuxPaneCleanupIntent {
    pub(super) operation_id: String,
    pub(super) run_id: String,
    pub(super) activation_id: String,
    pub(super) allocation_generation: u64,
    pub(super) pane: StoredPane,
    pub(super) reason: String,
}

impl RuntimeDriverService {
    pub(super) fn suspend_replayed_running_run(&mut self) -> Result<(), DriverFailure> {
        if !self.has_bound_run()
            || self.run_status() != runtime::RunStatus::Running
            || !self.driver.runtime().has_run(&self.config.run_id)
        {
            return Ok(());
        }
        let mut next_driver = self.driver.clone();
        next_driver
            .runtime_mut()
            .set_run_status_with_reason(
                &self.config.run_id,
                runtime::RunStatus::Paused,
                Some("driver_restarted"),
            )
            .map_err(DriverFailure::from_runtime)?;
        self.commit_runtime(next_driver)
    }

    pub(super) fn register_configured_operator_pane(&mut self) -> Result<(), DriverFailure> {
        let Some(config) = self.config.operator_pane.clone() else {
            return Ok(());
        };
        let pane = StoredPane::from_driver_config(&config);
        if self.operator_pane.as_ref() == Some(&pane) {
            return Ok(());
        }
        if self.operator_pane.is_some() {
            self.release_operator_pane("driver_replaced")?;
        }
        self.append_driver_event(
            "driver_pane_owned",
            json!({
                "pane": pane
            }),
        )?;
        self.operator_pane = Some(pane);
        Ok(())
    }

    pub(super) fn reconcile_stale_operator_pane(&mut self) -> Result<(), DriverFailure> {
        let Some(pane) = self.operator_pane.clone() else {
            return Ok(());
        };
        let metadata = TmuxActivationMetadata::new(
            &pane.session_id,
            &self.config.run_id,
            &pane.window_name,
            &pane.window_id,
            "driver",
            &pane.pane_id,
        );
        match self
            .tmux_adapter
            .probe_exact_pane_presence(&metadata)
            .map_err(DriverFailure::from_tmux)?
        {
            TmuxPanePresence::Present => Ok(()),
            TmuxPanePresence::Absent => self.persist_operator_pane_released(&pane, "stale_replay"),
        }
    }

    pub(super) fn publish_run_asset_flow_revision(
        &self,
        package: &StoredFlowRevision,
    ) -> Result<(), DriverFailure> {
        let mut manifest = self.load_run_asset_manifest()?;
        if manifest.flow.revisions.iter().any(|revision| {
            revision.flow_lock_id == package.lock_id()
                && revision.content_hash == package.content_hash()
                && revision.apply_state == "applied"
        }) {
            return Ok(());
        }
        if let Some(revision_id) = manifest
            .flow
            .revisions
            .iter()
            .find(|revision| {
                revision.flow_lock_id == package.lock_id()
                    && revision.content_hash == package.content_hash()
                    && revision.apply_state == "prepared"
            })
            .map(|revision| revision.revision_id.clone())
        {
            self.run_asset_store
                .commit_flow_revision_applied(&mut manifest, &revision_id)
                .map_err(DriverFailure::from_run_asset)?;
            return Ok(());
        }
        let lock = package.lock()?;
        let review_status = self
            .review_store
            .load(package.review_id())
            .map_err(|error| DriverFailure::new("review_invalid", error.to_string()))?
            .status()
            .as_str();
        self.run_asset_store
            .persist_flow_revision(&mut manifest, &lock, package.content_hash(), review_status)
            .map_err(DriverFailure::from_run_asset)?;
        Ok(())
    }

    pub(super) fn ensure_activation_capture_started(
        &mut self,
        activation: &runtime::Activation,
        adapter: &str,
        pane: &StoredPane,
    ) -> Result<String, DriverFailure> {
        let mut manifest = self.load_run_asset_manifest()?;
        if let Some(existing) = manifest.activations.get(&activation.activation_id)
            && (existing.pane_id.is_empty() || existing.pane_id == pane.pane_id)
            && existing.allocation_generation == pane.allocation_generation
            && existing.capture_phase == "capturing"
            && existing.pipe_acknowledged
            && self
                .tmux_pipe_captures
                .get(&activation.activation_id)
                .is_some_and(|(allocation_generation, _)| {
                    *allocation_generation == pane.allocation_generation
                })
        {
            if let Some(binding) = self
                .participant_bindings
                .get(&(activation.activation_id.clone(), pane.allocation_generation))
            {
                return Ok(binding.readiness_nonce.clone());
            }
            return self
                .run_asset_store
                .ensure_activation_readiness_nonce(&mut manifest, &activation.activation_id)
                .map_err(DriverFailure::from_run_asset);
        }

        let intent = match self.pending_pipe_captures.get(&activation.activation_id) {
            Some(intent) => {
                if intent.allocation_generation != pane.allocation_generation
                    || intent.pane != *pane
                {
                    return Err(DriverFailure::new(
                        "tmux_error",
                        "pending pipe capture belongs to a different pane allocation",
                    ));
                }
                intent.clone()
            }
            None => {
                let paths = self
                    .run_asset_store
                    .start_activation_capture(
                        &mut manifest,
                        RunAssetActivationUpdate {
                            activation_id: activation.activation_id.clone(),
                            node_id: activation.node_id.clone(),
                            tmux: RunAssetTmuxTarget {
                                session_id: pane.session_id.clone(),
                                window_id: pane.window_id.clone(),
                                window_name: pane.window_name.clone(),
                                pane_id: pane.pane_id.clone(),
                                allocation_generation: pane.allocation_generation,
                            },
                            adapter: adapter.to_string(),
                            termination_reason: None,
                        },
                    )
                    .map_err(DriverFailure::from_run_asset)?;
                let ack_relative_path = pipe_ack_relative_path(&paths.pipe_relative_path);
                let completion_relative_path = pipe_completion_relative_path(&ack_relative_path);
                let intent = TmuxPipeCaptureIntent {
                    operation_id: pipe_capture_operation_id(
                        &self.config.run_id,
                        &activation.activation_id,
                        pane,
                    ),
                    activation_id: activation.activation_id.clone(),
                    allocation_generation: pane.allocation_generation,
                    pane: pane.clone(),
                    transcript_relative_path: PathBuf::from(paths.pipe_relative_path),
                    transcript_identity: paths.pipe_identity,
                    ack_relative_path: PathBuf::from(ack_relative_path),
                    completion_relative_path: PathBuf::from(completion_relative_path),
                    ack_nonce: new_pipe_capture_nonce(),
                };
                self.append_driver_event(
                    "pipe_capture_intent",
                    serde_json::to_value(&intent).map_err(|err| {
                        DriverFailure::new("driver_storage_error", err.to_string())
                    })?,
                )?;
                self.pending_pipe_captures
                    .insert(activation.activation_id.clone(), intent.clone());
                intent
            }
        };
        let pane_handle = pane.tmux_pane(&activation.activation_id);
        let capture_root = self.run_asset_store.activation_capture_root(&manifest);
        let capture_request = PipeCaptureRequest {
            root: &capture_root,
            transcript_relative_path: &intent.transcript_relative_path,
            identity: &intent.transcript_identity,
            ack_relative_path: &intent.ack_relative_path,
            completion_relative_path: &intent.completion_relative_path,
            ack_nonce: &intent.ack_nonce,
            preserve_ready_ack: true,
        };
        let capture = self
            .tmux_adapter
            .start_pipe_capture_with_completion_nonce(&pane_handle, &capture_request)
            .map_err(DriverFailure::from_tmux)?;
        maybe_crash_after_tmux_effect("capture_started");
        let descriptor = capture.descriptor();
        self.append_driver_event(
            "pipe_capture_started",
            json!({
                "activation_id": activation.activation_id,
                "allocation_generation": pane.allocation_generation,
                "operation_id": intent.operation_id,
                "descriptor": descriptor
            }),
        )?;
        self.pending_pipe_captures.remove(&activation.activation_id);
        self.tmux_pipe_captures.insert(
            activation.activation_id.clone(),
            (pane.allocation_generation, capture),
        );
        self.run_asset_store
            .mark_activation_capture_acknowledged(&mut manifest, &activation.activation_id)
            .map_err(DriverFailure::from_run_asset)?;
        remove_pipe_sink_ack_under_root(&capture_root, &intent.ack_relative_path)
            .map_err(|err| DriverFailure::io("run_asset_error", err))?;
        self.run_asset_store
            .ensure_activation_readiness_nonce(&mut manifest, &activation.activation_id)
            .map_err(DriverFailure::from_run_asset)
    }

    pub(super) fn finalize_terminal_node_panes(
        &mut self,
        reason: &str,
    ) -> Result<Value, DriverFailure> {
        let activation_ids = self
            .tmux
            .as_ref()
            .map(|tmux| tmux.panes.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut finalized = Vec::new();
        let mut errors = Vec::new();
        let mut release_outcomes = Vec::new();
        for activation_id in activation_ids {
            match self.finalize_activation_pane(&activation_id, reason) {
                Ok(result) => finalized.push(result),
                Err(err) => {
                    release_outcomes.extend(err.tmux_release_outcomes());
                    errors.push(err.to_cleanup_json(Some(&activation_id)));
                }
            }
        }
        if errors.is_empty() {
            Ok(json!({
                "reason": reason,
                "finalized": finalized
            }))
        } else {
            let mut failure = DriverFailure::new(
                "tmux_cleanup_failed",
                "one or more driver-owned activation panes could not be finalized",
            );
            failure.extra = json!({
                "cleanup_errors": errors,
                "finalized": finalized
            });
            Err(failure.with_tmux_release_outcomes(release_outcomes))
        }
    }

    pub(super) fn finalize_for_exit(&mut self, reason: &str) -> Result<Value, DriverFailure> {
        self.suspend_run_for_driver_exit()?;

        let node_result = self.finalize_terminal_node_panes(reason);
        let operator_result = self.release_operator_pane(reason);
        match (node_result, operator_result) {
            (Ok(nodes), Ok(())) => Ok(json!({
                "reason": reason,
                "nodes": nodes,
                "operator_pane_released": true
            })),
            (nodes, operator) => {
                let mut errors = Vec::new();
                let mut release_outcomes = Vec::new();
                if let Err(err) = nodes {
                    release_outcomes.extend(err.tmux_release_outcomes());
                    errors.push(err.to_cleanup_json(None));
                }
                if let Err(err) = operator {
                    release_outcomes.extend(err.tmux_release_outcomes());
                    errors.push(err.to_cleanup_json(Some("driver")));
                }
                let mut failure = DriverFailure::new(
                    "tmux_cleanup_failed",
                    "driver shutdown could not finalize every owned pane",
                );
                failure.extra = json!({ "cleanup_errors": errors });
                Err(failure.with_tmux_release_outcomes(release_outcomes))
            }
        }
    }

    fn suspend_run_for_driver_exit(&mut self) -> Result<(), DriverFailure> {
        if !self.has_bound_run() || !self.driver.runtime().has_run(&self.config.run_id) {
            return Ok(());
        }
        if matches!(
            self.run_status(),
            runtime::RunStatus::Paused
                | runtime::RunStatus::Completed
                | runtime::RunStatus::Stopped
        ) {
            return Ok(());
        }
        let mut next_driver = self.driver.clone();
        next_driver.tick(
            DriverTickInput::default().with_control(ControlCommand::PauseRun {
                run_id: self.config.run_id.clone(),
            }),
        );
        self.commit_runtime(next_driver)
    }

    pub(super) fn finalize_activation_pane(
        &mut self,
        activation_id: &str,
        reason: &str,
    ) -> Result<Value, DriverFailure> {
        let pane = self
            .tmux
            .as_ref()
            .and_then(|tmux| tmux.panes.get(activation_id))
            .cloned()
            .ok_or_else(|| DriverFailure::new("tmux_error", "owned pane mapping is missing"))?;
        let intent = self.ensure_tmux_pane_cleanup_intent(activation_id, &pane, reason)?;
        let (result, preservation_error) = self.complete_tmux_pane_cleanup(&intent)?;
        if let Some(message) = preservation_error {
            return Err(DriverFailure::new("run_asset_error", message));
        }
        Ok(result)
    }

    pub(super) fn reconcile_pending_tmux_cleanups(&mut self) -> Result<(), DriverFailure> {
        let intents = self
            .pending_tmux_cleanups
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for intent in intents {
            self.complete_tmux_pane_cleanup(&intent)?;
        }
        Ok(())
    }

    pub(super) fn recover_activation_pane_cleanup(
        &mut self,
        activation_id: &str,
        reason: &str,
    ) -> Result<(), DriverFailure> {
        let pane = self
            .tmux
            .as_ref()
            .and_then(|tmux| tmux.panes.get(activation_id))
            .cloned()
            .ok_or_else(|| DriverFailure::new("tmux_error", "owned pane mapping is missing"))?;
        let intent = self.ensure_tmux_pane_cleanup_intent(activation_id, &pane, reason)?;
        self.complete_tmux_pane_cleanup(&intent)?;
        Ok(())
    }

    fn ensure_tmux_pane_cleanup_intent(
        &mut self,
        activation_id: &str,
        pane: &StoredPane,
        reason: &str,
    ) -> Result<TmuxPaneCleanupIntent, DriverFailure> {
        if let Some(intent) = self.pending_tmux_cleanups.get(activation_id) {
            if intent.run_id != self.config.run_id
                || intent.activation_id != activation_id
                || intent.allocation_generation != pane.allocation_generation
                || intent.pane != *pane
            {
                return Err(DriverFailure::new(
                    "tmux_error",
                    "pending pane cleanup belongs to a different allocation",
                ));
            }
            return Ok(intent.clone());
        }
        let intent = TmuxPaneCleanupIntent {
            operation_id: tmux_cleanup_operation_id(&self.config.run_id, activation_id, pane),
            run_id: self.config.run_id.clone(),
            activation_id: activation_id.to_string(),
            allocation_generation: pane.allocation_generation,
            pane: pane.clone(),
            reason: reason.to_string(),
        };
        self.append_driver_event(
            "tmux_pane_cleanup_intent",
            serde_json::to_value(&intent)
                .map_err(|err| DriverFailure::new("driver_storage_error", err.to_string()))?,
        )?;
        self.pending_tmux_cleanups
            .insert(activation_id.to_string(), intent.clone());
        Ok(intent)
    }

    fn complete_tmux_pane_cleanup(
        &mut self,
        intent: &TmuxPaneCleanupIntent,
    ) -> Result<(Value, Option<String>), DriverFailure> {
        if intent.run_id != self.config.run_id
            || intent.allocation_generation != intent.pane.allocation_generation
        {
            return Err(DriverFailure::new(
                "tmux_error",
                "pane cleanup intent does not match this driver run",
            ));
        }
        let current = self
            .tmux
            .as_ref()
            .and_then(|tmux| tmux.panes.get(&intent.activation_id));
        let owns_pane = match current {
            Some(current) if current != &intent.pane => {
                return Err(DriverFailure::new(
                    "tmux_error",
                    "pane cleanup intent does not match the current allocation",
                ));
            }
            Some(_) => true,
            None => false,
        };

        let mut manifest = self.load_run_asset_manifest()?;
        let existing = manifest
            .activations
            .get(&intent.activation_id)
            .filter(|activation| activation.allocation_generation == intent.allocation_generation)
            .cloned();
        if !owns_pane {
            if existing
                .as_ref()
                .is_some_and(|activation| activation.resource_cleanup_status != "complete")
            {
                return Err(DriverFailure::new(
                    "driver_storage_error",
                    "released pane cleanup has incomplete run assets",
                ));
            }
            let preservation_error = existing
                .as_ref()
                .and_then(|activation| activation.resource_cleanup_error.clone());
            self.retire_participant_binding(&intent.activation_id, intent.allocation_generation)?;
            return self.finish_tmux_pane_cleanup(intent, preservation_error);
        }

        let cleanup_already_recorded = existing
            .as_ref()
            .is_some_and(|activation| activation.resource_cleanup_status == "complete");
        let pipe_capture = self
            .tmux_pipe_captures
            .get(&intent.activation_id)
            .filter(|(allocation_generation, _)| {
                *allocation_generation == intent.allocation_generation
            })
            .map(|(_, capture)| capture.clone());
        let completion_already_published = pipe_capture
            .as_ref()
            .map(|capture| self.tmux_adapter.pipe_capture_completion_if_ready(capture))
            .transpose()
            .map_err(DriverFailure::from_tmux)?
            .flatten()
            .is_some();
        let metadata = TmuxActivationMetadata::new(
            &intent.pane.session_id,
            &intent.run_id,
            &intent.pane.window_name,
            &intent.pane.window_id,
            &intent.activation_id,
            &intent.pane.pane_id,
        );
        let captured = if cleanup_already_recorded {
            None
        } else if completion_already_published {
            existing
                .as_ref()
                .map(|activation| read_cleanup_capture(&activation.final_capture_path))
                .transpose()?
                .flatten()
        } else {
            match self
                .tmux_adapter
                .probe_exact_pane_presence(&metadata)
                .map_err(DriverFailure::from_tmux)?
            {
                TmuxPanePresence::Present => {
                    let pane_handle = intent.pane.tmux_pane(&intent.activation_id);
                    let captured = self
                        .tmux_adapter
                        .capture_pane(&pane_handle)
                        .map_err(DriverFailure::from_tmux)?;
                    if existing.is_some() {
                        self.run_asset_store
                            .persist_activation_final_capture_snapshot(
                                &manifest,
                                &intent.activation_id,
                                &captured,
                            )
                            .map_err(DriverFailure::from_run_asset)?;
                    }
                    if let Err(err) = self.tmux_adapter.kill_pane(&pane_handle) {
                        if existing.is_some() {
                            let _ = self.run_asset_store.mark_activation_resource_cleanup(
                                &mut manifest,
                                &intent.activation_id,
                                "failed",
                                Some(&err.to_string()),
                            );
                        }
                        return Err(DriverFailure::from_tmux(err));
                    }
                    maybe_crash_after_tmux_effect("pane_killed");
                    Some(captured)
                }
                TmuxPanePresence::Absent => existing
                    .as_ref()
                    .map(|activation| read_cleanup_capture(&activation.final_capture_path))
                    .transpose()?
                    .flatten(),
            }
        };

        let mut preservation_error = if cleanup_already_recorded {
            existing
                .as_ref()
                .and_then(|activation| activation.resource_cleanup_error.clone())
        } else {
            None
        };
        let mut completion_verified = completion_already_published;
        if !cleanup_already_recorded
            && let Some(activation) = existing
            && !(activation.capture_phase == "complete" && activation.capture_complete)
        {
            let completion = pipe_capture.as_ref().map(|capture| {
                if completion_already_published {
                    Ok(())
                } else {
                    self.tmux_adapter
                        .wait_for_pipe_capture_completion_preserve(capture)
                        .map(|_| ())
                }
            });
            completion_verified |= completion.as_ref().is_some_and(|result| result.is_ok());
            let result = match (completion, captured.as_deref()) {
                (Some(Ok(_)), Some(captured)) if activation.pipe_acknowledged => {
                    self.run_asset_store.complete_activation_capture(
                        &mut manifest,
                        &intent.activation_id,
                        &intent.reason,
                        captured,
                    )
                }
                (Some(Err(err)), captured) => {
                    preservation_error = Some(err.to_string());
                    self.run_asset_store.finalize_failed_activation_capture(
                        &mut manifest,
                        &intent.activation_id,
                        &intent.reason,
                        captured.unwrap_or_default(),
                    )
                }
                (_, captured) => {
                    preservation_error = Some(if captured.is_some() {
                        "pipe capture completion is unavailable".to_string()
                    } else {
                        "final capture snapshot is unavailable".to_string()
                    });
                    self.run_asset_store.finalize_failed_activation_capture(
                        &mut manifest,
                        &intent.activation_id,
                        &intent.reason,
                        captured.unwrap_or_default(),
                    )
                }
            };
            result.map_err(DriverFailure::from_run_asset)?;
            self.tmux_pipe_captures.remove(&intent.activation_id);
        }
        if manifest
            .activations
            .get(&intent.activation_id)
            .is_some_and(|activation| {
                activation.allocation_generation == intent.allocation_generation
                    && activation.resource_cleanup_status != "complete"
            })
        {
            self.run_asset_store
                .mark_activation_resource_cleanup(
                    &mut manifest,
                    &intent.activation_id,
                    "complete",
                    preservation_error.as_deref(),
                )
                .map_err(DriverFailure::from_run_asset)?;
        }
        if completion_verified && let Some(capture) = pipe_capture.as_ref() {
            self.tmux_adapter
                .remove_pipe_capture_completion(capture)
                .map_err(DriverFailure::from_tmux)?;
        }
        if self
            .tmux
            .as_ref()
            .and_then(|tmux| tmux.panes.get(&intent.activation_id))
            .is_some()
            && let Err(err) = self.release_tmux_panes(std::slice::from_ref(&intent.activation_id))
        {
            return Err(err.with_tmux_release_persistence_failure(
                intent.pane.to_json(&intent.activation_id),
            ));
        }
        self.retire_participant_binding(&intent.activation_id, intent.allocation_generation)?;
        self.finish_tmux_pane_cleanup(intent, preservation_error)
    }

    fn finish_tmux_pane_cleanup(
        &mut self,
        intent: &TmuxPaneCleanupIntent,
        preservation_error: Option<String>,
    ) -> Result<(Value, Option<String>), DriverFailure> {
        self.append_driver_event(
            "tmux_pane_cleanup_receipt",
            json!({
                "operation_id": intent.operation_id,
                "run_id": intent.run_id,
                "activation_id": intent.activation_id,
                "allocation_generation": intent.allocation_generation,
                "pane": intent.pane,
                "reason": intent.reason,
                "preservation_error": preservation_error
            }),
        )?;
        self.pending_tmux_cleanups.remove(&intent.activation_id);
        Ok((
            json!({
                "activation_id": intent.activation_id,
                "pane_id": intent.pane.pane_id,
                "status": "complete"
            }),
            preservation_error,
        ))
    }

    fn release_operator_pane(&mut self, reason: &str) -> Result<(), DriverFailure> {
        let Some(pane) = self.operator_pane.clone() else {
            return Ok(());
        };
        let pane_handle = pane.tmux_pane("driver");
        self.tmux_adapter
            .kill_pane(&pane_handle)
            .map_err(DriverFailure::from_tmux)?;
        if let Err(err) = self.persist_operator_pane_released(&pane, reason) {
            return Err(err.with_tmux_release_persistence_failure(pane.to_json("driver")));
        }
        Ok(())
    }

    fn persist_operator_pane_released(
        &mut self,
        pane: &StoredPane,
        reason: &str,
    ) -> Result<(), DriverFailure> {
        self.append_driver_event(
            "driver_pane_released",
            json!({
                "pane": pane,
                "reason": reason
            }),
        )?;
        self.operator_pane = None;
        Ok(())
    }

    pub(super) fn load_run_asset_manifest(
        &self,
    ) -> Result<crate::run_assets::RunAssetManifest, DriverFailure> {
        self.run_asset_store
            .load_manifest(&self.config.run_id)
            .map_err(DriverFailure::from_run_asset)
    }
}

impl StoredPane {
    fn from_driver_config(config: &DriverPaneConfig) -> Self {
        Self {
            pane_id: config.pane_id.clone(),
            session_id: config.session_id.clone(),
            window_id: config.window_id.clone(),
            window_name: config.window_name.clone(),
            allocation_generation: 0,
        }
    }

    fn tmux_pane(&self, activation_id: &str) -> TmuxPane {
        TmuxPane::new_in_session(
            self.session_id.clone(),
            self.window_id.clone(),
            activation_id.to_string(),
            self.pane_id.clone(),
        )
    }
}

impl DriverFailure {
    fn to_cleanup_json(&self, activation_id: Option<&str>) -> Value {
        json!({
            "activation_id": activation_id,
            "code": self.code,
            "message": self.message,
            "details": self.extra
        })
    }
}

fn pipe_ack_relative_path(pipe_relative_path: &str) -> String {
    let path = Path::new(pipe_relative_path);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("pipe");
    let ack_name = format!(".{file_name}.driver-ready-{}", std::process::id());
    path.parent()
        .map(|parent| parent.join(&ack_name))
        .unwrap_or_else(|| Path::new(&ack_name).to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

fn pipe_completion_relative_path(ack_relative_path: &str) -> String {
    let path = Path::new(ack_relative_path);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("pipe.ready");
    path.with_file_name(format!("{file_name}.complete"))
        .to_string_lossy()
        .replace('\\', "/")
}

fn pipe_capture_operation_id(run_id: &str, activation_id: &str, pane: &StoredPane) -> String {
    let mut hasher = Sha256::new();
    for value in [
        "humanize.tmux.pipe_capture.v1",
        run_id,
        activation_id,
        pane.pane_id.as_str(),
        pane.window_id.as_str(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.update(pane.allocation_generation.to_be_bytes());
    format!("capture-{:x}", hasher.finalize())
}

fn tmux_cleanup_operation_id(run_id: &str, activation_id: &str, pane: &StoredPane) -> String {
    let mut hasher = Sha256::new();
    for value in [
        "humanize.tmux.pane_cleanup.v1",
        run_id,
        activation_id,
        pane.session_id.as_str(),
        pane.window_id.as_str(),
        pane.window_name.as_str(),
        pane.pane_id.as_str(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.update(pane.allocation_generation.to_be_bytes());
    format!("cleanup-{:x}", hasher.finalize())
}

fn read_cleanup_capture(path: &Path) -> Result<Option<String>, DriverFailure> {
    read_regular_private(path)
        .map_err(DriverFailure::from_run_asset)?
        .map(String::from_utf8)
        .transpose()
        .map_err(|err| DriverFailure::new("run_asset_error", err.to_string()))
}
