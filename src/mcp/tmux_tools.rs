use crate::adapters::tmux::{CommandRunner, TmuxPane, TmuxSession, TmuxWindow};
use crate::runtime;
use serde_json::{Value, json};

use super::{McpServer, ToolError, tmux_start_options};

#[derive(Debug, Clone)]
pub(super) struct TmuxRunAllocation {
    pub(super) structured: Value,
    pub(super) window: Option<TmuxWindow>,
    pub(super) panes: Vec<TmuxPane>,
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
        &self,
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
                let session = TmuxSession::new(session_id);
                let mut panes = Vec::with_capacity(activation_ids.len());
                let window = match activation_ids.split_first() {
                    Some((first_activation_id, remaining_activation_ids)) => {
                        let (window, first_pane) = if self
                            .tmux_adapter
                            .has_session(&session)
                            .map_err(ToolError::from_tmux)?
                        {
                            self.tmux_adapter
                                .create_window_named_with_pane(
                                    &session,
                                    run_id,
                                    window_name.as_str(),
                                    first_activation_id.as_str(),
                                )
                                .map_err(ToolError::from_tmux)?
                        } else {
                            let (_, window, pane) = self
                                .tmux_adapter
                                .create_session_with_window_pane(
                                    session.id(),
                                    run_id,
                                    window_name.as_str(),
                                    first_activation_id.as_str(),
                                )
                                .map_err(ToolError::from_tmux)?;
                            (window, pane)
                        };
                        panes.push(first_pane);
                        for activation_id in remaining_activation_ids {
                            match self
                                .tmux_adapter
                                .split_pane_for_activation(&window, activation_id.as_str())
                            {
                                Ok(pane) => panes.push(pane),
                                Err(err) => {
                                    let tool_err = ToolError::from_tmux(err);
                                    let _ = self.tmux_adapter.kill_window(&window);
                                    return Err(tool_err);
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
        let mut allocated = Vec::new();
        for activation_id in activation_ids {
            let key = (run_id.to_string(), activation_id.clone());
            if self.state.tmux_panes.contains_key(&key) {
                continue;
            }
            let pane = self
                .tmux_adapter
                .split_pane_for_activation(&window, activation_id.as_str())
                .map_err(ToolError::from_tmux)?;
            let pane_json = tmux_pane_json(&window, &pane);
            self.state.tmux_panes.insert(key, pane);
            allocated.push(pane_json);
        }
        Ok(allocated)
    }

    pub(super) fn cleanup_tmux_pane_after_stop(
        &mut self,
        run_id: &str,
        activation_id: &str,
        report: &runtime::DriverTickReport,
    ) -> Result<Value, ToolError> {
        let allowed = report.stop_decisions.iter().any(|decision| {
            decision.kind == runtime::StopDecisionKind::Allow && decision.attempt > 0
        });
        if !allowed {
            return Ok(Value::Null);
        }
        let key = (run_id.to_string(), activation_id.to_string());
        let Some(pane) = self.state.tmux_panes.get(&key).cloned() else {
            return Ok(Value::Null);
        };
        self.tmux_adapter
            .kill_pane(&pane)
            .map_err(ToolError::from_tmux)?;
        self.state.tmux_panes.remove(&key);

        Ok(json!({
            "action": "kill_pane",
            "run_id": run_id,
            "activation_id": activation_id,
            "session_id": pane.session_id(),
            "window_id": pane.window_id(),
            "pane_id": pane.id()
        }))
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
