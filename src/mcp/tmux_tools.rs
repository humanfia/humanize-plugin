use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::adapters::tmux::{CommandRunner, TmuxPane, TmuxSession, TmuxWindow};
use crate::run_assets::{
    RunAssetActivationFailureUpdate, RunAssetActivationUpdate, RunAssetTmuxTarget,
};
use crate::runtime;
use serde_json::{Value, json};

use super::{McpServer, ToolError, tmux_start_options};

#[derive(Debug, Clone)]
pub(super) struct TmuxRunAllocation {
    pub(super) structured: Value,
    pub(super) window: Option<TmuxWindow>,
    pub(super) panes: Vec<TmuxPane>,
}

pub(super) struct TmuxCleanupResult {
    pub(super) structured: Value,
    pub(super) preservation_error: Option<Value>,
    pub(super) cleanup_error: Option<Value>,
}

impl TmuxRunAllocation {
    fn disabled() -> Self {
        Self {
            structured: json!({
                "enabled": false,
                "created": false
            }),
            window: None,
            panes: Vec::new(),
        }
    }
}

impl<R: CommandRunner> McpServer<R> {
    pub(super) fn start_run_tmux_metadata(
        &mut self,
        run_id: &str,
        arguments: &Value,
        activation_ids: &[String],
    ) -> Result<TmuxRunAllocation, ToolError> {
        match tmux_start_options(arguments)? {
            super::TmuxStartOptions::Disabled => Ok(TmuxRunAllocation::disabled()),
            super::TmuxStartOptions::Enabled {
                session_id,
                window_name,
            } => {
                let expected = activation_ids
                    .iter()
                    .map(|activation_id| (activation_id.clone(), activation_id.clone()))
                    .collect::<Vec<_>>();
                self.register_expected_tmux_activation_assets(run_id, &expected)?;
                let session = TmuxSession::new(session_id);
                let mut panes = Vec::with_capacity(activation_ids.len());
                let window = match activation_ids.split_first() {
                    Some((first_activation_id, remaining_activation_ids)) => {
                        let (window, first_pane) = if self
                            .tmux_adapter
                            .has_session(&session)
                            .map_err(ToolError::from_tmux)?
                        {
                            match self.tmux_adapter.create_window_named_with_pane(
                                &session,
                                run_id,
                                window_name.as_str(),
                                first_activation_id.as_str(),
                            ) {
                                Ok(created) => created,
                                Err(err) => {
                                    let message = err.to_string();
                                    self.record_asset_preservation_error(
                                        run_id,
                                        Some(first_activation_id.as_str()),
                                        Some("pane_allocation"),
                                        "pane_allocation",
                                        &message,
                                    );
                                    let _ = self
                                        .state
                                        .runtime_mut()
                                        .set_run_status(run_id, runtime::RunStatus::Failed);
                                    return Err(ToolError::from_tmux(err));
                                }
                            }
                        } else {
                            let (_, window, pane) =
                                match self.tmux_adapter.create_session_with_window_pane(
                                    session.id(),
                                    run_id,
                                    window_name.as_str(),
                                    first_activation_id.as_str(),
                                ) {
                                    Ok(created) => created,
                                    Err(err) => {
                                        let message = err.to_string();
                                        self.record_asset_preservation_error(
                                            run_id,
                                            Some(first_activation_id.as_str()),
                                            Some("pane_allocation"),
                                            "pane_allocation",
                                            &message,
                                        );
                                        let _ = self
                                            .state
                                            .runtime_mut()
                                            .set_run_status(run_id, runtime::RunStatus::Failed);
                                        return Err(ToolError::from_tmux(err));
                                    }
                                };
                            (window, pane)
                        };
                        self.state.remember_tmux_allocation(
                            run_id,
                            &Some(window.clone()),
                            std::slice::from_ref(&first_pane),
                        );
                        if let Err(err) = self.start_activation_asset_capture(
                            run_id,
                            first_activation_id.as_str(),
                            first_activation_id.as_str(),
                            &window,
                            &first_pane,
                        ) {
                            return Err(self.finalize_tmux_after_error(run_id, "pipe_start", err));
                        }
                        panes.push(first_pane);
                        for activation_id in remaining_activation_ids {
                            match self
                                .tmux_adapter
                                .split_pane_for_activation(&window, activation_id.as_str())
                            {
                                Ok(pane) => {
                                    self.state.remember_tmux_allocation(
                                        run_id,
                                        &Some(window.clone()),
                                        std::slice::from_ref(&pane),
                                    );
                                    if let Err(err) = self.start_activation_asset_capture(
                                        run_id,
                                        activation_id.as_str(),
                                        activation_id.as_str(),
                                        &window,
                                        &pane,
                                    ) {
                                        return Err(self.finalize_tmux_after_error(
                                            run_id,
                                            "pipe_start",
                                            err,
                                        ));
                                    }
                                    panes.push(pane);
                                }
                                Err(err) => {
                                    let message = err.to_string();
                                    self.record_asset_preservation_error(
                                        run_id,
                                        Some(activation_id.as_str()),
                                        Some("pane_allocation"),
                                        "pane_allocation",
                                        &message,
                                    );
                                    let _ = self
                                        .state
                                        .runtime_mut()
                                        .set_run_status(run_id, runtime::RunStatus::Failed);
                                    let tool_err = ToolError::from_tmux(err);
                                    return Err(self.finalize_tmux_after_error(
                                        run_id,
                                        "allocation_error",
                                        tool_err,
                                    ));
                                }
                            }
                        }
                        window
                    }
                    None => {
                        let session = self
                            .tmux_adapter
                            .ensure_session(session.id())
                            .map_err(ToolError::from_tmux)?;
                        self.tmux_adapter
                            .create_window_named(&session, run_id, window_name.as_str())
                            .map_err(ToolError::from_tmux)?
                    }
                };
                let pane_json = panes
                    .iter()
                    .map(|pane| tmux_pane_json(&window, pane))
                    .collect::<Vec<_>>();

                Ok(TmuxRunAllocation {
                    structured: json!({
                        "enabled": true,
                        "created": true,
                        "session_id": session.id(),
                        "window_id": window.id(),
                        "window_name": window.name(),
                        "run_id": window.run_id(),
                        "panes": pane_json
                    }),
                    window: Some(window),
                    panes,
                })
            }
        }
    }

    pub(super) fn allocate_missing_tmux_panes(
        &mut self,
        run_id: &str,
    ) -> Result<Vec<Value>, ToolError> {
        let Some(window) = self.state.tmux_windows.get(run_id).cloned() else {
            return Ok(Vec::new());
        };
        let activation_ids = self
            .state
            .runtime()
            .state()
            .activations
            .values()
            .filter(|activation| activation.run_id == run_id)
            .filter(|activation| self.activation_requires_tmux_capture(run_id, activation))
            .filter(|activation| {
                matches!(
                    activation.status,
                    runtime::ActivationStatus::Pending
                        | runtime::ActivationStatus::Starting
                        | runtime::ActivationStatus::Running
                        | runtime::ActivationStatus::WaitingForStop
                        | runtime::ActivationStatus::ValidatingStop
                )
            })
            .map(|activation| activation.activation_id.clone())
            .collect::<Vec<_>>();
        let expected = activation_ids
            .iter()
            .map(|activation_id| {
                let node_id = self
                    .state
                    .runtime()
                    .state()
                    .activations
                    .get(&(run_id.to_string(), activation_id.clone()))
                    .map(|activation| activation.node_id.clone())
                    .unwrap_or_else(|| activation_id.clone());
                (activation_id.clone(), node_id)
            })
            .collect::<Vec<_>>();
        self.register_expected_tmux_activation_assets(run_id, &expected)?;
        let mut allocated = Vec::new();
        for activation_id in activation_ids {
            let key = (run_id.to_string(), activation_id.clone());
            if self.state.tmux_panes.contains_key(&key) {
                continue;
            }
            let node_id = self
                .state
                .runtime()
                .state()
                .activations
                .get(&(run_id.to_string(), activation_id.clone()))
                .map(|activation| activation.node_id.clone())
                .unwrap_or_else(|| activation_id.clone());
            let mut last_err = None;
            let mut captured_pane = None;
            for _ in 0..2 {
                let pane = match self
                    .tmux_adapter
                    .split_pane_for_activation(&window, activation_id.as_str())
                {
                    Ok(pane) => pane,
                    Err(err) => {
                        let message = err.to_string();
                        self.record_asset_preservation_error(
                            run_id,
                            Some(&activation_id),
                            Some("pane_allocation"),
                            "pane_allocation",
                            &message,
                        );
                        let _ = self
                            .state
                            .runtime_mut()
                            .set_run_status(run_id, runtime::RunStatus::Failed);
                        return Err(ToolError::from_tmux(err));
                    }
                };
                self.state.remember_tmux_allocation(
                    run_id,
                    &Some(window.clone()),
                    std::slice::from_ref(&pane),
                );
                match self.start_activation_asset_capture(
                    run_id,
                    &activation_id,
                    &node_id,
                    &window,
                    &pane,
                ) {
                    Ok(_) => {
                        captured_pane = Some(pane);
                        break;
                    }
                    Err(err) => {
                        self.cleanup_tmux_pane_by_id(run_id, &activation_id, "pipe_start", &pane)?;
                        last_err = Some(err);
                    }
                }
            }
            let Some(pane) = captured_pane else {
                return Err(last_err.unwrap_or_else(|| ToolError::invalid("tmux capture failed")));
            };
            let pane_json = tmux_pane_json(&window, &pane);
            self.state.tmux_panes.insert(key, pane);
            allocated.push(pane_json);
        }
        Ok(allocated)
    }

    fn register_expected_tmux_activation_assets(
        &mut self,
        run_id: &str,
        expected: &[(String, String)],
    ) -> Result<(), ToolError> {
        if expected.is_empty() || !self.state.run_assets.contains_key(run_id) {
            return Ok(());
        }
        let manifest = self
            .state
            .run_assets
            .get_mut(run_id)
            .ok_or_else(|| ToolError::invalid("run asset manifest not found"))?;
        for (activation_id, node_id) in expected {
            if let Err(err) = self.run_asset_store.register_expected_activation(
                manifest,
                activation_id,
                node_id,
                "tmux",
            ) {
                let message = err.to_string();
                self.record_activation_store_failure(
                    run_id,
                    activation_id,
                    node_id,
                    None,
                    "activation_register",
                    &message,
                );
                let _ = self
                    .state
                    .runtime_mut()
                    .set_run_status(run_id, runtime::RunStatus::Failed);
                return Err(ToolError::from_run_asset(err));
            }
        }
        Ok(())
    }

    pub(super) fn capture_existing_tmux_panes(
        &mut self,
        run_id: &str,
    ) -> Result<Vec<Value>, ToolError> {
        let Some(window) = self.state.tmux_windows.get(run_id).cloned() else {
            return Ok(Vec::new());
        };
        let panes = self
            .state
            .tmux_panes
            .iter()
            .filter(|((pane_run_id, _), _)| pane_run_id == run_id)
            .map(|((_, activation_id), pane)| (activation_id.clone(), pane.clone()))
            .collect::<Vec<_>>();
        let mut captures = Vec::new();
        for (activation_id, pane) in panes {
            let node_id = self
                .state
                .runtime()
                .state()
                .activations
                .get(&(run_id.to_string(), activation_id.clone()))
                .map(|activation| activation.node_id.clone())
                .unwrap_or_else(|| activation_id.clone());
            if let Some(capture) = self.start_activation_asset_capture(
                run_id,
                &activation_id,
                &node_id,
                &window,
                &pane,
            )? {
                captures.push(capture);
            }
        }
        Ok(captures)
    }

    pub(super) fn cleanup_tmux_pane_after_stop(
        &mut self,
        run_id: &str,
        activation_id: &str,
        termination_reason: &str,
        report: &runtime::DriverTickReport,
    ) -> Result<TmuxCleanupResult, ToolError> {
        let allowed = report.stop_decisions.iter().any(|decision| {
            decision.kind == runtime::StopDecisionKind::Allow && decision.attempt > 0
        });
        if !allowed {
            return Ok(TmuxCleanupResult {
                structured: Value::Null,
                preservation_error: None,
                cleanup_error: None,
            });
        }
        let key = (run_id.to_string(), activation_id.to_string());
        let Some(pane) = self.state.tmux_panes.get(&key).cloned() else {
            return Ok(TmuxCleanupResult {
                structured: Value::Null,
                preservation_error: None,
                cleanup_error: None,
            });
        };
        Ok(self.finalize_and_release_tmux_pane(run_id, activation_id, termination_reason, &pane))
    }

    pub(super) fn cleanup_all_tmux_panes_for_run(
        &mut self,
        run_id: &str,
        termination_reason: &str,
    ) -> Result<TmuxCleanupResult, ToolError> {
        let panes = self
            .state
            .tmux_panes
            .iter()
            .filter(|((pane_run_id, _), _)| pane_run_id == run_id)
            .map(|((_, activation_id), pane)| (activation_id.clone(), pane.clone()))
            .collect::<Vec<_>>();
        if panes.is_empty() {
            return Ok(TmuxCleanupResult {
                structured: json!({
                    "run_id": run_id,
                    "activations": [],
                    "cleanup_errors": []
                }),
                preservation_error: None,
                cleanup_error: None,
            });
        }

        let mut activations = Vec::new();
        let mut first_preservation_error = None;
        let mut cleanup_errors = Vec::new();
        for (activation_id, pane) in panes {
            let cleanup = self.finalize_and_release_tmux_pane(
                run_id,
                &activation_id,
                termination_reason,
                &pane,
            );
            if first_preservation_error.is_none() {
                first_preservation_error = cleanup.preservation_error.clone();
            }
            if let Some(error) = cleanup.cleanup_error.clone() {
                cleanup_errors.push(error);
            }
            activations.push(cleanup.structured);
        }

        let cleanup_error = cleanup_errors.first().cloned();

        Ok(TmuxCleanupResult {
            structured: json!({
                "run_id": run_id,
                "activations": activations,
                "cleanup_errors": cleanup_errors
            }),
            preservation_error: first_preservation_error,
            cleanup_error,
        })
    }

    pub(super) fn finalize_tmux_after_error(
        &mut self,
        run_id: &str,
        termination_reason: &str,
        original: ToolError,
    ) -> ToolError {
        match self.cleanup_all_tmux_panes_for_run(run_id, termination_reason) {
            Ok(cleanup) => {
                let mut message = original.message;
                if cleanup.preservation_error.is_some() {
                    message.push_str("; run asset preservation failed");
                }
                if cleanup.cleanup_error.is_some() {
                    message.push_str("; tmux resource cleanup failed");
                }
                ToolError::invalid(message)
            }
            Err(cleanup) => ToolError::invalid(format!(
                "{}; tmux resource cleanup failed: {}",
                original.message, cleanup.message
            )),
        }
    }

    fn cleanup_tmux_pane_by_id(
        &mut self,
        run_id: &str,
        activation_id: &str,
        termination_reason: &str,
        pane: &TmuxPane,
    ) -> Result<TmuxCleanupResult, ToolError> {
        let cleanup =
            self.finalize_and_release_tmux_pane(run_id, activation_id, termination_reason, pane);
        if cleanup.cleanup_error.is_some() {
            return Err(ToolError::invalid("tmux resource cleanup failed"));
        }
        Ok(TmuxCleanupResult {
            structured: json!({
                "run_id": run_id,
                "activations": [cleanup.structured],
                "cleanup_errors": []
            }),
            preservation_error: cleanup.preservation_error,
            cleanup_error: None,
        })
    }

    pub(super) fn shutdown_active_tmux_assets(
        &mut self,
        termination_reason: &str,
    ) -> Result<Value, ToolError> {
        if self.state.shutdown_assets_finalized {
            if self.state.shutdown_assets_error.is_some()
                && !self.state.released_tmux_panes.is_empty()
            {
                self.state.shutdown_assets_finalized = false;
                self.state.shutdown_assets_error = None;
                self.state.shutdown_assets_summary = None;
            } else {
                if let Some(error) = &self.state.shutdown_assets_error {
                    return Err(ToolError::invalid(error.clone()));
                }
                return Ok(self
                    .state
                    .shutdown_assets_summary
                    .clone()
                    .unwrap_or_else(|| json!({ "runs": [] })));
            }
        }
        self.state.shutdown_assets_finalized = true;
        let mut run_ids = self
            .state
            .tmux_panes
            .keys()
            .map(|(run_id, _)| run_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        run_ids.extend(
            self.state
                .run_assets
                .iter()
                .filter(|(_, manifest)| !manifest.activations.is_empty())
                .map(|(run_id, _)| run_id.clone()),
        );
        let mut runs = Vec::new();
        let mut failed = false;
        for run_id in run_ids {
            match self.cleanup_all_tmux_panes_for_run(&run_id, termination_reason) {
                Ok(cleanup) => {
                    failed |=
                        cleanup.preservation_error.is_some() || cleanup.cleanup_error.is_some();
                    runs.push(cleanup.structured);
                }
                Err(err) => {
                    failed = true;
                    runs.push(json!({
                        "run_id": run_id,
                        "error": err.message
                    }));
                }
            }
        }
        let remaining_panes = self.state.tmux_panes.len();
        let incomplete_activations = self
            .state
            .run_assets
            .values()
            .map(|manifest| manifest.completion.incomplete_tmux_activations.len())
            .sum::<usize>();
        let manifest_preservation_failed = self.state.run_assets.values().any(|manifest| {
            manifest.preservation_blocked
                || !manifest.preservation_errors.is_empty()
                || !manifest.completion.incomplete_tmux_activations.is_empty()
        });
        failed |= manifest_preservation_failed;
        let summary = json!({
            "runs": runs,
            "remaining_panes": remaining_panes,
            "incomplete_activations": incomplete_activations
        });
        self.state.shutdown_assets_summary = Some(summary.clone());
        if failed || remaining_panes > 0 {
            let message = if remaining_panes > 0 {
                format!("tmux resource cleanup failed: {remaining_panes} pane(s) remain")
            } else {
                format!(
                    "tmux asset preservation incomplete: {incomplete_activations} activation(s)"
                )
            };
            self.state.shutdown_assets_error = Some(message.clone());
            Err(ToolError::invalid(message))
        } else {
            Ok(summary)
        }
    }

    fn finalize_and_release_tmux_pane(
        &mut self,
        run_id: &str,
        activation_id: &str,
        termination_reason: &str,
        pane: &TmuxPane,
    ) -> TmuxCleanupResult {
        let key = (run_id.to_string(), activation_id.to_string());
        let capture_terminal = self
            .state
            .run_assets
            .get(run_id)
            .and_then(|manifest| manifest.activations.get(activation_id))
            .map(|activation| {
                let complete = activation.preservation_status == "complete"
                    && activation.capture_complete
                    && activation.final_capture_path.is_file();
                let failed = activation.preservation_status == "failed"
                    && activation.final_capture_path.is_file();
                activation.ended_at_ms.is_some() && (complete || failed)
            })
            .unwrap_or(true);
        let mut snapshot_error = None;
        if !capture_terminal && !self.state.tmux_final_captures.contains_key(&key) {
            match self.tmux_adapter.capture_pane(pane) {
                Ok(capture) => {
                    let persist_result = self.state.run_assets.get(run_id).map(|manifest| {
                        self.run_asset_store
                            .persist_activation_final_capture_snapshot(
                                manifest,
                                activation_id,
                                &capture,
                            )
                    });
                    match persist_result {
                        Some(Ok(_)) => {
                            self.state.tmux_final_captures.insert(key.clone(), capture);
                        }
                        Some(Err(err)) => {
                            let message = err.to_string();
                            self.record_asset_preservation_error(
                                run_id,
                                Some(activation_id),
                                Some(termination_reason),
                                "final_capture",
                                &message,
                            );
                            snapshot_error = Some(json!({
                                "status": "failed",
                                "activation_id": activation_id,
                                "stage": "final_capture",
                                "error": message
                            }));
                        }
                        None => {
                            let message = "run asset manifest not found";
                            snapshot_error = Some(json!({
                                "status": "failed",
                                "activation_id": activation_id,
                                "stage": "final_capture",
                                "error": message
                            }));
                        }
                    }
                }
                Err(err) => {
                    let message = err.to_string();
                    self.record_asset_preservation_error(
                        run_id,
                        Some(activation_id),
                        Some(termination_reason),
                        "final_capture",
                        &message,
                    );
                    snapshot_error = Some(json!({
                        "status": "failed",
                        "activation_id": activation_id,
                        "stage": "final_capture",
                        "error": message
                    }));
                }
            }
        }

        let already_released = self.state.released_tmux_panes.contains(&key);
        let kill_result = if already_released {
            Ok(())
        } else {
            self.tmux_adapter.kill_pane(pane)
        };
        if kill_result.is_ok() {
            self.state.released_tmux_panes.insert(key.clone());
        }

        let pipe_completion = if capture_terminal || kill_result.is_err() {
            None
        } else {
            self.state
                .tmux_pipe_captures
                .get(&key)
                .cloned()
                .map(|pipe_capture| {
                    self.tmux_adapter
                        .wait_for_pipe_capture_completion(&pipe_capture)
                })
        };
        let asset_preservation = if capture_terminal {
            self.existing_activation_asset_result(run_id, activation_id)
        } else if let Some(error) = snapshot_error {
            if let Some(Err(err)) = pipe_completion {
                self.record_asset_preservation_error(
                    run_id,
                    Some(activation_id),
                    Some(termination_reason),
                    "pipe_completion",
                    &err.to_string(),
                );
            }
            Err(error)
        } else if kill_result.is_err() {
            self.incomplete_activation_asset_result(run_id, activation_id)
        } else {
            let snapshot = self.state.tmux_final_captures.get(&key).cloned();
            match (snapshot, pipe_completion) {
                (Some(snapshot), Some(Ok(_))) => self.complete_activation_asset_capture(
                    run_id,
                    activation_id,
                    termination_reason,
                    &snapshot,
                ),
                (Some(snapshot), Some(Err(err))) => self.fail_activation_asset_capture(
                    run_id,
                    activation_id,
                    termination_reason,
                    "pipe_completion",
                    &err.to_string(),
                    Some(&snapshot),
                ),
                (Some(snapshot), None) => self.fail_activation_asset_capture(
                    run_id,
                    activation_id,
                    termination_reason,
                    "pipe_completion",
                    "pipe sink completion handle is missing",
                    Some(&snapshot),
                ),
                (None, _) => Err(json!({
                    "status": "failed",
                    "activation_id": activation_id,
                    "stage": "final_capture",
                    "error": "final pane capture is unavailable"
                })),
            }
        };
        let preservation_error = asset_preservation.as_ref().err().cloned();
        let preservation_json = match asset_preservation {
            Ok(value) | Err(value) => value,
        };

        let (resource_cleanup_status, cleanup_error) = match kill_result {
            Ok(()) => {
                let status_error =
                    self.persist_resource_cleanup_status(run_id, activation_id, "complete", None);
                match status_error {
                    None => {
                        self.state.tmux_panes.remove(&key);
                        self.state.tmux_pipe_captures.remove(&key);
                        self.state.tmux_final_captures.remove(&key);
                        self.state.released_tmux_panes.remove(&key);
                        ("complete", None)
                    }
                    Some(error) => ("failed", Some(error)),
                }
            }
            Err(err) => {
                let message = err.to_string();
                let persistence_error = self.persist_resource_cleanup_status(
                    run_id,
                    activation_id,
                    "failed",
                    Some(&message),
                );
                let cleanup_error = json!({
                    "activation_id": activation_id,
                    "stage": "kill_pane",
                    "error": message,
                    "resource_cleanup_persistence": persistence_error
                });
                ("failed", Some(cleanup_error))
            }
        };
        if cleanup_error.is_some() || preservation_error.is_some() {
            let _ = self
                .state
                .runtime_mut()
                .set_run_status(run_id, runtime::RunStatus::Failed);
        }

        TmuxCleanupResult {
            structured: json!({
                "action": "kill_pane",
                "status": resource_cleanup_status,
                "resource_cleanup_status": resource_cleanup_status,
                "run_id": run_id,
                "activation_id": activation_id,
                "session_id": pane.session_id(),
                "window_id": pane.window_id(),
                "pane_id": pane.id(),
                "asset_preservation": preservation_json
            }),
            preservation_error,
            cleanup_error,
        }
    }

    fn existing_activation_asset_result(
        &self,
        run_id: &str,
        activation_id: &str,
    ) -> Result<Value, Value> {
        let Some(activation) = self
            .state
            .run_assets
            .get(run_id)
            .and_then(|manifest| manifest.activations.get(activation_id))
        else {
            return Ok(Value::Null);
        };
        let value = json!({
            "status": activation.preservation_status,
            "activation_id": activation.activation_id,
            "capture_complete": activation.capture_complete,
            "final_capture_path": activation.final_capture_path
        });
        if activation.preservation_status == "complete" && activation.capture_complete {
            Ok(value)
        } else {
            Err(value)
        }
    }

    fn incomplete_activation_asset_result(
        &self,
        run_id: &str,
        activation_id: &str,
    ) -> Result<Value, Value> {
        let Some(activation) = self
            .state
            .run_assets
            .get(run_id)
            .and_then(|manifest| manifest.activations.get(activation_id))
        else {
            return Ok(Value::Null);
        };
        let value = json!({
            "status": activation.preservation_status,
            "activation_id": activation.activation_id,
            "capture_complete": activation.capture_complete,
            "final_capture_path": activation.final_capture_path
        });
        if activation.preservation_status == "failed" {
            Err(value)
        } else {
            Ok(value)
        }
    }

    fn persist_resource_cleanup_status(
        &mut self,
        run_id: &str,
        activation_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Option<Value> {
        let manifest = self.state.run_assets.get_mut(run_id)?;
        self.run_asset_store
            .mark_activation_resource_cleanup(manifest, activation_id, status, error)
            .err()
            .map(|err| {
                json!({
                    "activation_id": activation_id,
                    "stage": "resource_cleanup_manifest",
                    "error": err.to_string()
                })
            })
    }

    fn activation_requires_tmux_capture(
        &self,
        run_id: &str,
        _activation: &runtime::Activation,
    ) -> bool {
        self.state.tmux_windows.contains_key(run_id)
    }

    fn start_activation_asset_capture(
        &mut self,
        run_id: &str,
        activation_id: &str,
        node_id: &str,
        window: &TmuxWindow,
        pane: &TmuxPane,
    ) -> Result<Option<Value>, ToolError> {
        if !self.state.run_assets.contains_key(run_id) {
            return Ok(None);
        }
        let tmux = RunAssetTmuxTarget {
            session_id: pane.session_id().to_string(),
            window_id: pane.window_id().to_string(),
            window_name: window.name().to_string(),
            pane_id: pane.id().to_string(),
        };
        let target = tmux.target();
        if self
            .state
            .run_assets
            .get(run_id)
            .and_then(|manifest| manifest.activations.get(activation_id))
            .map(|activation| {
                activation.tmux_target == target
                    && activation.preservation_status == "capturing"
                    && !activation.capture_complete
            })
            .unwrap_or(false)
        {
            return Ok(None);
        }
        let (asset_root, paths) = {
            let manifest = self
                .state
                .run_assets
                .get_mut(run_id)
                .ok_or_else(|| ToolError::invalid("run asset manifest not found"))?;
            let paths = match self.run_asset_store.start_activation_capture(
                manifest,
                RunAssetActivationUpdate {
                    activation_id: activation_id.to_string(),
                    node_id: node_id.to_string(),
                    adapter: "tmux".to_string(),
                    tmux: tmux.clone(),
                    termination_reason: None,
                },
            ) {
                Ok(paths) => paths,
                Err(err) => {
                    let message = err.to_string();
                    self.record_activation_store_failure(
                        run_id,
                        activation_id,
                        node_id,
                        Some(tmux.clone()),
                        "activation_capture",
                        &message,
                    );
                    let _ = self
                        .state
                        .runtime_mut()
                        .set_run_status(run_id, runtime::RunStatus::Failed);
                    return Err(ToolError::from_run_asset(err));
                }
            };
            (manifest.root.clone(), paths)
        };
        let ack_relative_path = pipe_ack_relative_path(&paths.pipe_relative_path);
        let completion_relative_path = pipe_completion_relative_path(&ack_relative_path);
        let pipe_capture = match self.tmux_adapter.start_pipe_capture_with_completion(
            pane,
            &asset_root,
            &paths.pipe_relative_path,
            &paths.pipe_identity,
            &ack_relative_path,
            &completion_relative_path,
        ) {
            Ok(capture) => capture,
            Err(err) => {
                let message = err.to_string();
                self.record_asset_preservation_error(
                    run_id,
                    Some(activation_id),
                    Some("pipe_start"),
                    "pipe_start",
                    &message,
                );
                let _ = self
                    .state
                    .runtime_mut()
                    .set_run_status(run_id, runtime::RunStatus::Failed);
                return Err(ToolError::from_tmux(err));
            }
        };
        self.state.tmux_pipe_captures.insert(
            (run_id.to_string(), activation_id.to_string()),
            pipe_capture,
        );
        if let Err(err) = {
            let manifest = self
                .state
                .run_assets
                .get_mut(run_id)
                .ok_or_else(|| ToolError::invalid("run asset manifest not found"))?;
            self.run_asset_store
                .mark_activation_capture_acknowledged(manifest, activation_id)
                .map_err(ToolError::from_run_asset)
        } {
            let message = err.message.clone();
            self.record_activation_store_failure(
                run_id,
                activation_id,
                node_id,
                Some(tmux),
                "pipe_acknowledge",
                &message,
            );
            let _ = self
                .state
                .runtime_mut()
                .set_run_status(run_id, runtime::RunStatus::Failed);
            return Err(err);
        }

        Ok(Some(json!({
            "status": "capturing",
            "activation_id": activation_id,
            "node_id": node_id,
            "pipe_path": paths.pipe_path
        })))
    }

    fn complete_activation_asset_capture(
        &mut self,
        run_id: &str,
        activation_id: &str,
        termination_reason: &str,
        final_capture: &str,
    ) -> Result<Value, Value> {
        if !self
            .state
            .run_assets
            .get(run_id)
            .map(|manifest| manifest.activations.contains_key(activation_id))
            .unwrap_or(false)
        {
            return Ok(Value::Null);
        }
        if let Some(activation) = self
            .state
            .run_assets
            .get(run_id)
            .and_then(|manifest| manifest.activations.get(activation_id))
            .filter(|activation| {
                activation.capture_phase == "complete"
                    && activation.capture_complete
                    && activation.preservation_status == "complete"
            })
        {
            return Ok(json!({
                "status": activation.preservation_status,
                "activation_id": activation.activation_id,
                "capture_complete": activation.capture_complete,
                "final_capture_path": activation.final_capture_path
            }));
        }
        let already_failed = self
            .state
            .run_assets
            .get(run_id)
            .and_then(|manifest| manifest.activations.get(activation_id))
            .map(|activation| activation.preservation_status == "failed")
            .unwrap_or(false);
        let result = {
            let manifest = self
                .state
                .run_assets
                .get_mut(run_id)
                .expect("checked run asset manifest should exist");
            if already_failed {
                self.run_asset_store.finalize_failed_activation_capture(
                    manifest,
                    activation_id,
                    termination_reason,
                    final_capture,
                )
            } else {
                self.run_asset_store.complete_activation_capture(
                    manifest,
                    activation_id,
                    termination_reason,
                    final_capture,
                )
            }
        };
        match result {
            Ok(activation) => {
                let value = json!({
                    "status": activation.preservation_status,
                    "activation_id": activation.activation_id,
                    "capture_complete": activation.capture_complete,
                    "final_capture_path": activation.final_capture_path
                });
                if activation.preservation_status == "complete" && activation.capture_complete {
                    Ok(value)
                } else {
                    Err(value)
                }
            }
            Err(err) => {
                let message = err.to_string();
                self.record_asset_preservation_error(
                    run_id,
                    Some(activation_id),
                    Some(termination_reason),
                    "final_capture",
                    &message,
                );
                let _ = self
                    .state
                    .runtime_mut()
                    .set_run_status(run_id, runtime::RunStatus::Failed);
                Err(json!({
                    "status": "failed",
                    "activation_id": activation_id,
                    "stage": "final_capture",
                    "error": message
                }))
            }
        }
    }

    fn fail_activation_asset_capture(
        &mut self,
        run_id: &str,
        activation_id: &str,
        termination_reason: &str,
        stage: &str,
        message: &str,
        final_capture: Option<&str>,
    ) -> Result<Value, Value> {
        if let Some(final_capture) = final_capture
            && let Some(manifest) = self.state.run_assets.get_mut(run_id)
        {
            let _ = self.run_asset_store.finalize_failed_activation_capture(
                manifest,
                activation_id,
                termination_reason,
                final_capture,
            );
        }
        self.record_asset_preservation_error(
            run_id,
            Some(activation_id),
            Some(termination_reason),
            stage,
            message,
        );
        let _ = self
            .state
            .runtime_mut()
            .set_run_status(run_id, runtime::RunStatus::Failed);
        Err(json!({
            "status": "failed",
            "activation_id": activation_id,
            "stage": stage,
            "error": message
        }))
    }

    pub(super) fn record_asset_preservation_error(
        &mut self,
        run_id: &str,
        activation_id: Option<&str>,
        termination_reason: Option<&str>,
        stage: &str,
        message: &str,
    ) {
        if let Some(manifest) = self.state.run_assets.get_mut(run_id) {
            let _ = self.run_asset_store.record_preservation_error(
                manifest,
                activation_id,
                termination_reason,
                stage,
                message,
            );
        }
    }

    fn record_activation_store_failure(
        &mut self,
        run_id: &str,
        activation_id: &str,
        node_id: &str,
        tmux: Option<RunAssetTmuxTarget>,
        stage: &str,
        message: &str,
    ) {
        if let Some(manifest) = self.state.run_assets.get_mut(run_id) {
            let _ = self.run_asset_store.record_activation_store_failure(
                manifest,
                RunAssetActivationFailureUpdate {
                    activation_id: activation_id.to_string(),
                    node_id: node_id.to_string(),
                    tmux,
                    adapter: "tmux".to_string(),
                    termination_reason: Some(stage.to_string()),
                    stage: stage.to_string(),
                    error: message.to_string(),
                },
            );
        }
    }
}

fn tmux_pane_json(window: &TmuxWindow, pane: &TmuxPane) -> Value {
    json!({
        "activation_id": pane.activation_id(),
        "pane_id": pane.id(),
        "session_id": pane.session_id(),
        "window_id": window.id(),
        "window_name": window.name()
    })
}

fn pipe_ack_relative_path(pipe_relative_path: &str) -> String {
    let path = Path::new(pipe_relative_path);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("pipe");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let ack_name = format!(".{file_name}.ready-{}-{nonce}", std::process::id());
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
