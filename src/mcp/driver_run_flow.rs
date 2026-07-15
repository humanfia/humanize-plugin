use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::adapters::tmux::{
    CommandRunner, TmuxActivationMetadata, TmuxInputTransactionConfig, TmuxPane, TmuxPanePresence,
};
use crate::driver::{
    DriverClient, cleanup_stale_driver_ipc, private_driver_dir, runtime_root_for_run_root,
};
use crate::flow;
use crate::input_ledger::MachineInputLedger;
use serde_json::{Value, json};

use super::driver_cleanup::{
    cleanup_panes_from_response, persist_private_mcp_diagnostic,
    physically_released_panes_from_response, publish_cleanup_report,
    sanitize_public_driver_response,
};
use super::{McpServer, ToolCallResult, ToolError, optional_string, optional_u64, require_string};

const DRIVER_READY_TIMEOUT: Duration = Duration::from_secs(5);

fn agent_command_from_arguments(arguments: &Value) -> Result<Option<String>, ToolError> {
    if let Some(command) = optional_string(arguments, &["agent_command", "agentCommand"])? {
        return non_empty_agent_command(command, "agent_command");
    }
    let Some(tmux) = arguments.get("tmux") else {
        return Ok(None);
    };
    let object = tmux
        .as_object()
        .ok_or_else(|| ToolError::invalid("tmux must be an object"))?;
    for key in ["agent_command", "agentCommand"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        let Some(command) = value.as_str() else {
            return Err(ToolError::invalid("tmux.agent_command must be a string"));
        };
        return non_empty_agent_command(command, "tmux.agent_command");
    }
    Ok(None)
}

fn non_empty_agent_command(command: &str, field: &str) -> Result<Option<String>, ToolError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(ToolError::invalid(format!("{field} must be non-empty")));
    }
    Ok(Some(command.to_string()))
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum ExplicitTmuxArguments {
    Absent,
    Disabled,
    Enabled {
        session: Option<String>,
        window: Option<String>,
    },
}

impl ExplicitTmuxArguments {
    fn session(&self) -> Option<String> {
        match self {
            Self::Enabled { session, .. } => session.clone(),
            Self::Absent | Self::Disabled => None,
        }
    }

    fn window(&self) -> Option<String> {
        match self {
            Self::Enabled { window, .. } => window.clone(),
            Self::Absent | Self::Disabled => None,
        }
    }
}

fn explicit_tmux_arguments(arguments: &Value) -> Result<ExplicitTmuxArguments, ToolError> {
    let Some(value) = arguments.get("tmux") else {
        return Ok(ExplicitTmuxArguments::Absent);
    };
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("tmux must be an object"))?;
    let enabled = match object.get("enabled") {
        Some(Value::Bool(enabled)) => *enabled,
        Some(_) => return Err(ToolError::invalid("tmux.enabled must be a boolean")),
        None => false,
    };
    if !enabled {
        return Ok(ExplicitTmuxArguments::Disabled);
    }

    Ok(ExplicitTmuxArguments::Enabled {
        session: super::optional_string_field(object, &["session", "session_id", "sessionId"])?
            .map(str::to_string),
        window: super::optional_string_field(object, &["window", "window_name", "windowName"])?
            .map(str::to_string),
    })
}

fn node_driver_name(driver: flow::NodeDriver) -> &'static str {
    match driver {
        flow::NodeDriver::Agent => "agent",
        flow::NodeDriver::Script => "script",
        flow::NodeDriver::Review => "review",
        flow::NodeDriver::Human => "human",
    }
}

fn draft_requires_autonomous_tmux(draft: &flow::FlowDraft) -> bool {
    draft.nodes.iter().any(|node| {
        node.action.as_ref().is_some_and(|action| {
            matches!(
                action.driver,
                flow::NodeDriver::Agent | flow::NodeDriver::Review
            )
        })
    })
}

fn unsupported_autonomous_action_nodes(draft: &flow::FlowDraft) -> Vec<Value> {
    draft
        .nodes
        .iter()
        .filter_map(|node| {
            let action = node.action.as_ref()?;
            (action.driver == flow::NodeDriver::Script).then(|| {
                json!({
                    "node_id": node.id.as_str(),
                    "driver": node_driver_name(action.driver)
                })
            })
        })
        .collect()
}

impl<R: CommandRunner> McpServer<R> {
    fn effective_run_flow_arguments(
        &self,
        run_id: &str,
        arguments: &Value,
        flow_binding: Option<&(String, String)>,
    ) -> Result<Result<Value, ToolCallResult>, ToolError> {
        let Some((lock_id, _)) = flow_binding else {
            return Ok(Ok(arguments.clone()));
        };
        let Some(lock) = self.state.flow_locks.get(lock_id) else {
            return Err(ToolError::invalid("flow lock not found"));
        };
        let unsupported = unsupported_autonomous_action_nodes(lock.draft());
        if !unsupported.is_empty() {
            return Ok(Err(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "flow_lock_id": lock_id,
                "error": "locked flow contains action drivers that are not autonomously supported",
                "unsupported_action_drivers": unsupported
            }))));
        }
        if !draft_requires_autonomous_tmux(lock.draft()) {
            return Ok(Ok(arguments.clone()));
        }

        match self.autonomous_tmux_arguments(run_id, arguments)? {
            Ok(arguments) => Ok(Ok(arguments)),
            Err(missing) => Ok(Err(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "flow_lock_id": lock_id,
                "error": "autonomous tmux execution context required",
                "missing": missing
            })))),
        }
    }

    fn autonomous_tmux_arguments(
        &self,
        run_id: &str,
        arguments: &Value,
    ) -> Result<Result<Value, Vec<&'static str>>, ToolError> {
        let explicit = explicit_tmux_arguments(arguments)?;
        if explicit == ExplicitTmuxArguments::Disabled {
            return Ok(Err(vec!["tmux.enabled"]));
        }
        let mut session = explicit.session();
        let mut window = explicit.window();
        let mut agent_command = agent_command_from_arguments(arguments)?;

        if session.is_none() {
            session = self.execution_defaults.session.clone();
        }
        if window.is_none() {
            window = self.execution_defaults.window.clone();
        }
        if agent_command.is_none() {
            agent_command = self.execution_defaults.agent_command.clone();
        }

        let mut missing = Vec::new();
        if session
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            missing.push("tmux.session");
        }
        if agent_command
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            missing.push("tmux.agent_command");
        }
        if !missing.is_empty() {
            return Ok(Err(missing));
        }

        let mut effective_arguments = arguments.clone();
        let object = effective_arguments
            .as_object_mut()
            .ok_or_else(|| ToolError::invalid("run_flow arguments must be an object"))?;
        let mut tmux = arguments
            .get("tmux")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        crate::driver::parse_tmux_actuation_config(&tmux).map_err(ToolError::invalid)?;
        tmux.insert("enabled".into(), Value::Bool(true));
        tmux.insert(
            "session".into(),
            Value::String(session.expect("session should be present after validation")),
        );
        tmux.insert(
            "window".into(),
            Value::String(window.unwrap_or_else(|| run_id.to_string())),
        );
        tmux.insert(
            "agent_command".into(),
            Value::String(agent_command.expect("agent command should be present after validation")),
        );
        object.insert("tmux".into(), Value::Object(tmux));
        Ok(Ok(effective_arguments))
    }

    pub(super) fn run_flow_with_driver(
        &mut self,
        arguments: &Value,
    ) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let run_root = self
            .run_asset_store
            .run_root(run_id)
            .map_err(ToolError::from_run_asset)?;
        let run_mode = requested_run_mode(arguments)?;
        let activation_limit =
            optional_u64(arguments, &["activation_limit", "activationLimit"])?.unwrap_or(u64::MAX);
        let stop_attempt_limit =
            optional_u64(arguments, &["stop_attempt_limit", "stopAttemptLimit"])?.unwrap_or(3);
        let review_id = require_string(arguments, &["review_id", "reviewId"])?;
        let attached = match self.attach_or_restart_driver(run_id) {
            Ok(attached) => attached,
            Err(err) => {
                persist_private_mcp_diagnostic(
                    &run_root,
                    "driver_recovery_failed",
                    json!({ "error": err.diagnostic() }),
                );
                return Err(err);
            }
        };
        if let Some(attached) = attached.as_ref()
            && attached
                .status
                .get("run_mode")
                .and_then(Value::as_str)
                .is_some()
        {
            let mut bind_request = json!({
                "run_mode": run_mode,
                "activation_limit": activation_limit,
                "stop_attempt_limit": stop_attempt_limit,
                "review_id": review_id
            });
            if let Some((lock_id, provided_content_hash)) =
                self.flow_lock_binding_from_arguments(arguments)?
            {
                let lock = self
                    .state
                    .flow_locks
                    .get(&lock_id)
                    .ok_or_else(|| ToolError::invalid("flow lock not found"))?;
                let expected_content_hash = lock.content_hash();
                if provided_content_hash != expected_content_hash {
                    return Ok(ToolCallResult::error(json!({
                        "ok": false,
                        "run_id": run_id,
                        "flow_lock_id": lock_id,
                        "content_hash": provided_content_hash,
                        "expected_content_hash": expected_content_hash,
                        "error": "flow lock content hash mismatch"
                    })));
                }
                bind_request["flow_lock"] = flow_lock_package(lock, &provided_content_hash)?;
            }
            let mut bind = attached
                .client
                .request("bind_run", run_id, &bind_request)
                .map_err(|err| {
                    persist_private_mcp_diagnostic(
                        &run_root,
                        "driver_bind_failed",
                        json!({ "error": err.to_string() }),
                    );
                    ToolError::private_failure("runtime driver bind is unavailable", err)
                })?;
            if bind.get("ok").and_then(Value::as_bool) != Some(true) {
                sanitize_public_driver_response(&mut bind);
                return Ok(ToolCallResult::error(bind));
            }
            let status = attached
                .client
                .request("status", run_id, &json!({}))
                .map_err(|err| {
                    persist_private_mcp_diagnostic(
                        &run_root,
                        "driver_status_failed",
                        json!({ "error": err.to_string() }),
                    );
                    ToolError::private_failure("runtime driver status is unavailable", err)
                })?;
            return Ok(run_flow_attached_result(run_id, status));
        }
        let bootstrap_client = attached.map(|attached| attached.client);
        let flow_binding = self.flow_lock_binding_from_arguments(arguments)?;
        let Some((lock_id, provided_content_hash)) = flow_binding else {
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "error": "flow_lock_id is required for driver run_flow"
            })));
        };
        let lock = self
            .state
            .flow_locks
            .get(&lock_id)
            .ok_or_else(|| ToolError::invalid("flow lock not found"))?
            .clone();
        let expected_content_hash = lock.content_hash();
        if provided_content_hash != expected_content_hash {
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "flow_lock_id": lock_id,
                "content_hash": provided_content_hash,
                "expected_content_hash": expected_content_hash,
                "error": "flow lock content hash mismatch"
            })));
        }

        let effective_arguments = match self.effective_run_flow_arguments(
            run_id,
            arguments,
            Some(&(lock_id.clone(), provided_content_hash.clone())),
        )? {
            Ok(arguments) => arguments,
            Err(result) => return Ok(result),
        };
        let agent_command = agent_command_from_arguments(&effective_arguments)?
            .ok_or_else(|| ToolError::invalid("tmux.agent_command is required"))?;
        let runs_root = self
            .run_asset_store
            .runs_root()
            .map_err(ToolError::from_run_asset)?;
        let token_path = private_driver_dir(
            &runtime_root_for_run_root(&run_root).map_err(ToolError::from_io)?,
            &run_root,
        )
        .join("ipc-token");
        let (client, driver) = match bootstrap_client {
            Some(client) => (client, None),
            None => {
                let token = generate_driver_token()?;
                if run_root.join("manifest.json").exists()
                    && let Err(err) = cleanup_stale_driver_ipc(&run_root, run_id)
                    && err.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(ToolError::from_io(err));
                }
                let token_path = write_driver_token(&run_root, &token)?;
                let driver = match self.launch_driver_pane(
                    run_id,
                    &effective_arguments,
                    &run_root,
                    &runs_root,
                    &token_path,
                ) {
                    Ok(driver) => driver,
                    Err(err) => {
                        persist_private_mcp_diagnostic(
                            &run_root,
                            "driver_launch_stage_failed",
                            json!({ "error": err.message }),
                        );
                        if let Err(cleanup_err) =
                            cleanup_driver_ipc_artifacts(&run_root, &token_path, run_id)
                        {
                            persist_private_mcp_diagnostic(
                                &run_root,
                                "driver_ipc_cleanup_failed",
                                json!({ "error": cleanup_err.message }),
                            );
                            return Err(ToolError::invalid(
                                "runtime driver launch failed; startup cleanup incomplete",
                            ));
                        }
                        return Err(ToolError::invalid("runtime driver launch failed"));
                    }
                };
                let client = match wait_for_driver_client(&run_root, run_id) {
                    Ok(client) => client,
                    Err(err) => {
                        self.cleanup_driver_startup(
                            &run_root,
                            &token_path,
                            run_id,
                            None,
                            Some(&driver),
                            None,
                        )?;
                        return Err(err);
                    }
                };
                (client, Some(driver))
            }
        };
        let tmux = effective_arguments
            .get("tmux")
            .and_then(Value::as_object)
            .ok_or_else(|| ToolError::invalid("tmux execution context required"))?;
        crate::driver::parse_tmux_actuation_config(tmux).map_err(ToolError::invalid)?;
        let mut bind_tmux = Value::Object(tmux.clone());
        bind_tmux["enabled"] = Value::Bool(true);
        bind_tmux["session"] = tmux.get("session").cloned().unwrap_or(Value::Null);
        bind_tmux["window"] = Value::String(
            tmux.get("window")
                .and_then(Value::as_str)
                .unwrap_or(run_id)
                .to_string(),
        );
        bind_tmux["agent_command"] = Value::String(agent_command);
        if let Some(driver) = &driver {
            bind_tmux["window_id"] = Value::String(driver.window_id.clone());
        }
        let bind_request = json!({
            "flow_lock": flow_lock_package(&lock, &provided_content_hash)?,
            "review_id": review_id,
            "run_mode": run_mode,
            "activation_limit": activation_limit,
            "stop_attempt_limit": stop_attempt_limit,
            "tmux": bind_tmux
        });
        let mut bind = match client.request("bind_run", run_id, &bind_request) {
            Ok(bind) => bind,
            Err(err) => {
                persist_private_mcp_diagnostic(
                    &run_root,
                    "driver_bind_failed",
                    json!({ "error": err.to_string() }),
                );
                self.cleanup_driver_startup(
                    &run_root,
                    &token_path,
                    run_id,
                    Some(&client),
                    driver.as_ref(),
                    None,
                )?;
                return Err(ToolError::private_failure(
                    "runtime driver bind is unavailable",
                    err,
                ));
            }
        };
        let ok = bind.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if !ok {
            self.cleanup_driver_startup(
                &run_root,
                &token_path,
                run_id,
                Some(&client),
                driver.as_ref(),
                Some(&mut bind),
            )?;
            return Ok(ToolCallResult::error(bind));
        }
        let status = client
            .request("status", run_id, &json!({}))
            .map_err(|err| {
                persist_private_mcp_diagnostic(
                    &run_root,
                    "driver_status_failed",
                    json!({ "error": err.to_string() }),
                );
                ToolError::private_failure("runtime driver status is unavailable", err)
            })?;

        let mut response = json!({
            "ok": true,
            "run_id": run_id,
            "flow_lock_id": lock_id,
            "content_hash": provided_content_hash,
            "activation_ids": bind.get("activation_ids").cloned().unwrap_or(Value::Null),
            "run_mode": bind.get("run_mode").cloned().unwrap_or(Value::Null),
            "initial_activation_limit": bind
                .get("initial_activation_limit")
                .cloned()
                .unwrap_or(Value::Null),
            "stop_attempt_limit": bind
                .get("stop_attempt_limit")
                .cloned()
                .unwrap_or(Value::Null),
            "activation_limit": status.get("activation_limit").cloned().unwrap_or(Value::Null),
            "activations_used": status.get("activations_used").cloned().unwrap_or(Value::Null),
            "tmux": bind.get("tmux").cloned().unwrap_or(Value::Null),
            "run_status": status.get("run_status").cloned().unwrap_or(Value::Null),
            "event_cursor": status.get("event_cursor").cloned().unwrap_or(Value::Null),
            "context_generation": status
                .get("context_generation")
                .cloned()
                .unwrap_or(Value::Null)
        });
        sanitize_public_driver_response(&mut response);
        Ok(ToolCallResult::ok(response))
    }

    fn launch_driver_pane(
        &mut self,
        run_id: &str,
        arguments: &Value,
        run_root: &Path,
        runs_root: &Path,
        token_path: &Path,
    ) -> Result<DriverPane, ToolError> {
        let tmux = arguments
            .get("tmux")
            .and_then(Value::as_object)
            .ok_or_else(|| ToolError::invalid("tmux execution context required"))?;
        let session_id = tmux
            .get("session")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::invalid("tmux.session is required"))?;
        let window_name = tmux.get("window").and_then(Value::as_str).unwrap_or(run_id);
        self.launch_driver_pane_for_context(
            run_id,
            session_id,
            window_name,
            run_root,
            runs_root,
            token_path,
        )
    }

    pub(super) fn launch_driver_pane_for_context(
        &mut self,
        run_id: &str,
        session_id: &str,
        window_name: &str,
        run_root: &Path,
        runs_root: &Path,
        token_path: &Path,
    ) -> Result<DriverPane, ToolError> {
        let session = crate::adapters::tmux::TmuxSession::new(session_id);
        let (window, pane) = if self
            .tmux_adapter
            .has_session(&session)
            .map_err(ToolError::from_tmux)?
        {
            self.tmux_adapter
                .create_window_named_with_pane(&session, run_id, window_name, "driver")
                .map_err(ToolError::from_tmux)?
        } else {
            let (_, window, pane) = self
                .tmux_adapter
                .create_session_with_window_pane(session_id, run_id, window_name, "driver")
                .map_err(ToolError::from_tmux)?;
            (window, pane)
        };
        let executable = driver_executable_path();
        let runtime_root = runtime_root_for_run_root(run_root).map_err(ToolError::from_io)?;
        let command = format!(
            "{} --run-id {} --runs-root {} --runtime-root {} --auth-token-file {} --review-root {} --driver-session {} --driver-window-id {} --driver-window-name {} --driver-pane-id {}",
            shell_quote(&executable.to_string_lossy()),
            shell_quote(run_id),
            shell_quote(&runs_root.to_string_lossy()),
            shell_quote(&runtime_root.to_string_lossy()),
            shell_quote(&token_path.to_string_lossy()),
            shell_quote(&self.review_store.root().to_string_lossy()),
            shell_quote(window.session_id()),
            shell_quote(window.id()),
            shell_quote(window.name()),
            shell_quote(pane.id())
        );
        let metadata = TmuxActivationMetadata::new(
            window.session_id(),
            run_id,
            window.name(),
            window.id(),
            "driver",
            pane.id(),
        );
        let launch_input_config =
            TmuxInputTransactionConfig::runtime_with_ledger(MachineInputLedger::at_path(
                private_driver_dir(&runtime_root, run_root).join("machine-inputs.jsonl"),
            ));
        if let Err(err) = self.tmux_adapter.send_input_transaction_with_config(
            &metadata,
            &command,
            &launch_input_config,
        ) {
            let owned_pane =
                TmuxPane::new_in_session(window.session_id(), window.id(), "driver", pane.id());
            persist_private_mcp_diagnostic(
                run_root,
                "driver_launch_failed",
                json!({
                    "error": err.to_string(),
                    "pane": {
                        "session_id": owned_pane.session_id(),
                        "window_id": owned_pane.window_id(),
                        "pane_id": owned_pane.id()
                    }
                }),
            );
            let mut cleanup = json!({});
            let cleanup = self.cleanup_driver_response_panes(
                run_root,
                &mut cleanup,
                &[owned_pane],
                "driver_launch_failure",
            );
            let message = if cleanup.failed == 0 {
                "runtime driver launch failed"
            } else {
                "runtime driver launch failed; operator pane cleanup incomplete"
            };
            return Err(ToolError::invalid(message));
        }
        Ok(DriverPane {
            session_id: window.session_id().to_string(),
            window_id: window.id().to_string(),
            pane_id: pane.id().to_string(),
        })
    }

    pub(super) fn cleanup_driver_startup(
        &mut self,
        run_root: &Path,
        token_path: &Path,
        run_id: &str,
        client: Option<&DriverClient>,
        driver: Option<&DriverPane>,
        response: Option<&mut Value>,
    ) -> Result<(), ToolError> {
        let operator_panes = driver
            .map(|driver| {
                TmuxPane::new_in_session(
                    driver.session_id.clone(),
                    driver.window_id.clone(),
                    "driver",
                    driver.pane_id.clone(),
                )
            })
            .into_iter()
            .collect::<Vec<_>>();
        let has_response = response.is_some();
        let mut scratch = json!({});
        let target = response.unwrap_or(&mut scratch);
        let response_panes = cleanup_panes_from_response(target);
        let physically_released = physically_released_panes_from_response(target);
        let owned_before = self.durable_run_panes(run_id);
        if let Err(err) = &owned_before {
            persist_private_mcp_diagnostic(
                run_root,
                "driver_ownership_read_failed",
                json!({ "stage": "before_shutdown", "error": err.message }),
            );
        }
        let reconnect = if client.is_none() {
            DriverClient::from_run_root_for_run(run_root, run_id)
                .ok()
                .flatten()
        } else {
            None
        };
        let graceful_shutdown = client
            .or(reconnect.as_ref())
            .is_some_and(|client| request_driver_shutdown(client, run_root, run_id));
        let owned_after = if graceful_shutdown {
            self.durable_run_panes(run_id)
        } else {
            Ok(Vec::new())
        };
        if let Err(err) = &owned_after {
            persist_private_mcp_diagnostic(
                run_root,
                "driver_ownership_read_failed",
                json!({ "stage": "after_shutdown", "error": err.message }),
            );
        }

        let mut fallback_panes = response_panes
            .iter()
            .filter(|pane| {
                !contains_pane(&physically_released, pane)
                    && match &owned_before {
                        Ok(owned) => !contains_pane(owned, pane),
                        Err(_) => true,
                    }
            })
            .cloned()
            .collect::<Vec<_>>();
        if !graceful_shutdown {
            fallback_panes.extend(
                operator_panes
                    .iter()
                    .filter(|pane| {
                        !contains_pane(&physically_released, pane)
                            && match &owned_before {
                                Ok(owned) => !contains_pane(owned, pane),
                                Err(_) => true,
                            }
                    })
                    .cloned(),
            );
        }
        let mut deduped_fallback_panes = Vec::new();
        for pane in fallback_panes {
            if !contains_pane(&deduped_fallback_panes, &pane) {
                deduped_fallback_panes.push(pane);
            }
        }
        let mut fallback_panes = deduped_fallback_panes;

        let durable_candidates = if graceful_shutdown {
            owned_after.as_ref().ok()
        } else {
            owned_before.as_ref().ok()
        };
        let mut probe_failures = Vec::new();
        if let Some(candidates) = durable_candidates {
            for pane in candidates {
                if contains_pane(&physically_released, pane) {
                    continue;
                }
                match self.tmux_adapter.probe_pane_presence(pane) {
                    Ok(TmuxPanePresence::Present) => fallback_panes.push(pane.clone()),
                    Ok(TmuxPanePresence::Absent) => {}
                    Err(err) => {
                        let persisted = persist_private_mcp_diagnostic(
                            run_root,
                            "driver_pane_validation_failed",
                            json!({
                                "activation_id": pane.activation_id(),
                                "session_id": pane.session_id(),
                                "window_id": pane.window_id(),
                                "pane_id": pane.id(),
                                "error": err.to_string()
                            }),
                        );
                        probe_failures.push(persisted);
                    }
                }
            }
        } else {
            probe_failures.push(false);
        }
        let mut deduped_fallback_panes = Vec::new();
        for pane in fallback_panes {
            if !contains_pane(&deduped_fallback_panes, &pane) {
                deduped_fallback_panes.push(pane);
            }
        }
        let fallback_panes = deduped_fallback_panes;
        let mut cleanup =
            self.cleanup_driver_panes(run_root, target, &fallback_panes, "driver_startup_failure");
        for persisted in probe_failures {
            cleanup.add_action(true, persisted);
        }
        if let Err(err) = wait_for_driver_shutdown(run_root) {
            let persisted = persist_private_mcp_diagnostic(
                run_root,
                "driver_shutdown_wait_failed",
                json!({ "error": err.message }),
            );
            cleanup.add_action(true, persisted);
        }
        if let Err(err) = cleanup_driver_ipc_artifacts(run_root, token_path, run_id) {
            let persisted = persist_private_mcp_diagnostic(
                run_root,
                "driver_ipc_cleanup_failed",
                json!({
                    "run_root": run_root,
                    "error": err.message
                }),
            );
            cleanup.add_action(true, persisted);
        }
        if has_response {
            publish_cleanup_report(target, cleanup);
        }
        if !has_response && cleanup.failed > 0 {
            return Err(ToolError::invalid("driver startup cleanup failed"));
        }
        Ok(())
    }

    fn durable_run_panes(&self, run_id: &str) -> Result<Vec<TmuxPane>, ToolError> {
        self.run_asset_store
            .discover_private_owned_tmux_panes()
            .map_err(ToolError::from_run_asset)
            .map(|owned| {
                owned
                    .into_iter()
                    .filter(|pane| pane.run_id == run_id)
                    .map(|pane| {
                        TmuxPane::new_in_session(
                            pane.session_id,
                            pane.window_id,
                            pane.activation_id,
                            pane.pane_id,
                        )
                    })
                    .collect()
            })
    }
}

fn requested_run_mode(arguments: &Value) -> Result<&str, ToolError> {
    let mode = optional_string(arguments, &["run_mode", "runMode"])?.unwrap_or("finite");
    if matches!(mode, "finite" | "continuous" | "manual") {
        Ok(mode)
    } else {
        Err(ToolError::invalid(format!("unknown run mode: {mode}")))
    }
}

pub(super) fn flow_lock_package(
    lock: &flow::FlowLock,
    content_hash: &str,
) -> Result<Value, ToolError> {
    if content_hash != lock.content_hash() {
        return Err(ToolError::invalid("flow lock content hash mismatch"));
    }
    serde_json::to_value(lock).map_err(|_| ToolError::invalid("flow lock serialization failed"))
}

fn run_flow_attached_result(run_id: &str, status: Value) -> ToolCallResult {
    let context = status.get("context").cloned().unwrap_or(Value::Null);
    let activation_ids = context
        .get("activations")
        .and_then(Value::as_object)
        .map(|activations| activations.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let revision = context
        .get("flow_revisions")
        .and_then(Value::as_array)
        .and_then(|revisions| {
            revisions
                .iter()
                .filter_map(|revision| {
                    revision
                        .get("event_sequence")
                        .and_then(Value::as_u64)
                        .map(|sequence| (sequence, revision))
                })
                .max_by_key(|(sequence, _)| *sequence)
                .map(|(_, revision)| revision)
        });
    let mut response = json!({
        "ok": true,
        "run_id": run_id,
        "attached": true,
        "flow_lock_id": revision.and_then(|revision| revision.get("flow_lock_id")),
        "content_hash": revision.and_then(|revision| revision.get("content_hash")),
        "activation_ids": activation_ids,
        "run_mode": status.get("run_mode").cloned().unwrap_or(Value::Null),
        "initial_activation_limit": status
            .get("initial_activation_limit")
            .cloned()
            .unwrap_or(Value::Null),
        "stop_attempt_limit": status
            .get("stop_attempt_limit")
            .cloned()
            .unwrap_or(Value::Null),
        "activation_limit": status.get("activation_limit").cloned().unwrap_or(Value::Null),
        "activations_used": status.get("activations_used").cloned().unwrap_or(Value::Null),
        "run_status": status.get("run_status").cloned().unwrap_or(Value::Null),
        "event_cursor": status.get("event_cursor").cloned().unwrap_or(Value::Null),
        "context_generation": status
            .get("context_generation")
            .cloned()
            .unwrap_or(Value::Null)
    });
    sanitize_public_driver_response(&mut response);
    ToolCallResult::ok(response)
}

pub(super) struct DriverPane {
    session_id: String,
    window_id: String,
    pane_id: String,
}

fn wait_for_driver_shutdown(run_root: &Path) -> Result<(), ToolError> {
    let metadata_path = private_driver_dir(
        &runtime_root_for_run_root(run_root).map_err(ToolError::from_io)?,
        run_root,
    )
    .join("ipc.json");
    let started = Instant::now();
    loop {
        if !metadata_path.exists() {
            return Ok(());
        }
        if started.elapsed() >= DRIVER_READY_TIMEOUT {
            return Err(ToolError::invalid(
                "runtime driver did not stop during startup cleanup",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn request_driver_shutdown(client: &DriverClient, run_root: &Path, run_id: &str) -> bool {
    let result = client.request("shutdown", run_id, &json!({}));
    let Ok(response) = result else {
        persist_private_mcp_diagnostic(
            run_root,
            "driver_shutdown_request_failed",
            json!({ "error": "driver shutdown request failed" }),
        );
        return false;
    };
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        persist_private_mcp_diagnostic(
            run_root,
            "driver_shutdown_request_failed",
            json!({ "error": response.get("error").cloned() }),
        );
    }
    match wait_for_driver_shutdown(run_root) {
        Ok(()) => true,
        Err(err) => {
            persist_private_mcp_diagnostic(
                run_root,
                "driver_shutdown_wait_failed",
                json!({ "error": err.message }),
            );
            false
        }
    }
}

fn contains_pane(panes: &[TmuxPane], pane: &TmuxPane) -> bool {
    panes.iter().any(|candidate| {
        candidate.session_id() == pane.session_id()
            && candidate.window_id() == pane.window_id()
            && candidate.id() == pane.id()
    })
}

pub(super) fn wait_for_driver_client(
    run_root: &Path,
    run_id: &str,
) -> Result<DriverClient, ToolError> {
    let started = Instant::now();
    let timeout = driver_ready_timeout();
    loop {
        match DriverClient::from_run_root_for_run(run_root, run_id) {
            Ok(Some(client)) => match client.request("status", run_id, &json!({})) {
                Ok(response)
                    if response.get("ok").and_then(Value::as_bool) == Some(true)
                        && response.get("run_id").and_then(Value::as_str) == Some(run_id) =>
                {
                    return Ok(client);
                }
                Ok(_) | Err(_) => {
                    cleanup_stale_driver_ipc(run_root, run_id).map_err(ToolError::from_io)?;
                }
            },
            Ok(None) => {}
            Err(_) => {
                cleanup_stale_driver_ipc(run_root, run_id).map_err(ToolError::from_io)?;
            }
        }
        if started.elapsed() >= timeout {
            return Err(ToolError::invalid("runtime driver did not become ready"));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn driver_ready_timeout() -> Duration {
    std::env::var("HUMANIZE_DRIVER_READY_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or(DRIVER_READY_TIMEOUT)
}

fn cleanup_driver_ipc_artifacts(
    run_root: &Path,
    token_path: &Path,
    run_id: &str,
) -> Result<(), ToolError> {
    let driver_dir = private_driver_dir(
        &runtime_root_for_run_root(run_root).map_err(ToolError::from_io)?,
        run_root,
    );
    if !token_path.starts_with(&driver_dir) {
        return Err(ToolError::invalid(
            "driver IPC token path is outside driver directory",
        ));
    }
    if run_root.join("manifest.json").exists() {
        match cleanup_stale_driver_ipc(run_root, run_id) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
                remove_private_regular_file(&driver_dir.join("ipc.json"))?;
                remove_private_regular_file(token_path)?;
            }
            Err(err) => return Err(ToolError::from_io(err)),
        }
        return Ok(());
    }
    remove_private_regular_file(&driver_dir.join("ipc.json"))?;
    remove_private_regular_file(token_path)
}

fn remove_private_regular_file(path: &Path) -> Result<(), ToolError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::remove_file(path).map_err(ToolError::from_io)?;
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(ToolError::from_io(err)),
    }
    Ok(())
}

fn driver_executable_path() -> PathBuf {
    if let Some(path) = std::env::var_os("HUMANIZE_DRIVER_BIN")
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    let Some(current_exe) = std::env::current_exe().ok() else {
        return PathBuf::from("humanize-plugin-driver");
    };
    let Some(parent) = current_exe.parent() else {
        return PathBuf::from("humanize-plugin-driver");
    };
    let sibling = parent.join("humanize-plugin-driver");
    if sibling.exists() {
        return sibling;
    }
    let Some(debug_dir) = parent.parent() else {
        return PathBuf::from("humanize-plugin-driver");
    };
    let debug_sibling = debug_dir.join("humanize-plugin-driver");
    if debug_sibling.exists() {
        return debug_sibling;
    }
    PathBuf::from("humanize-plugin-driver")
}

pub(super) fn write_driver_token(run_root: &Path, token: &str) -> Result<PathBuf, ToolError> {
    let runtime_root = runtime_root_for_run_root(run_root).map_err(ToolError::from_io)?;
    let private_run_root = crate::private_state::ensure_private_run_root(&runtime_root, run_root)
        .map_err(ToolError::from_io)?;
    let driver_dir = private_run_root.join("driver");
    crate::private_state::ensure_private_directory(&driver_dir).map_err(ToolError::from_io)?;
    let path = driver_dir.join("ipc-token");
    crate::run_assets::write_create_new_private(&path, format!("{token}\n").as_bytes()).map_err(
        |err| ToolError::private_failure("runtime driver bootstrap storage is unavailable", err),
    )?;
    Ok(path)
}

pub(super) fn generate_driver_token() -> Result<String, ToolError> {
    let mut bytes = [0u8; 32];
    OpenOptions::new()
        .read(true)
        .open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(ToolError::from_io)?;
    Ok(hex_bytes(&bytes))
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attached_result_selects_latest_revision_by_numeric_event_sequence() {
        let status = json!({
            "context": {
                "activations": {},
                "flow_revisions": [
                    {
                        "revision_id": "flow-lock-application:10",
                        "event_sequence": 10,
                        "flow_lock_id": "lock-10",
                        "content_hash": "hash-10"
                    },
                    {
                        "revision_id": "flow-lock-application:2",
                        "event_sequence": 2,
                        "flow_lock_id": "lock-2",
                        "content_hash": "hash-2"
                    },
                    {
                        "revision_id": "flow-lock-application:11",
                        "event_sequence": 11,
                        "flow_lock_id": "lock-11",
                        "content_hash": "hash-11"
                    },
                    {
                        "revision_id": "flow-lock-application:9",
                        "event_sequence": 9,
                        "flow_lock_id": "lock-9",
                        "content_hash": "hash-9"
                    }
                ]
            },
            "run_mode": "continuous",
            "initial_activation_limit": 4,
            "activation_limit": 4,
            "activations_used": 0,
            "run_status": "quiescent",
            "event_cursor": 11,
            "context_generation": 11
        });

        let result = run_flow_attached_result("run-revisions", status);

        assert_eq!(result.structured["flow_lock_id"], "lock-11");
        assert_eq!(result.structured["content_hash"], "hash-11");
    }
}
