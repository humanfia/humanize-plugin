use std::collections::{BTreeMap, HashSet};

use crate::adapters::tmux::{CommandRunner, TmuxActivationMetadata};
use crate::flow;
use crate::input_ledger::MachineInputRecord;
use crate::runtime::{self, ControlCommand};
use serde_json::{Value, json};

use super::{
    FlowReviewStatus, McpServer, RunArchive, ToolCallResult, ToolError, content_hash,
    diagnostic_codes_text, flow_check_mode_arg, flow_draft_arg, node_specs, optional_bool,
    optional_string, require_string, run_not_found_guidance, validate_start_run_preconditions,
};

impl<R: CommandRunner> McpServer<R> {
    pub(super) fn run_flow(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let review_required = optional_bool(arguments, &["review_required", "reviewRequired"])?
            .unwrap_or_else(|| {
                arguments.get("flow").is_some()
                    || arguments.get("flow_lock_id").is_some()
                    || arguments.get("flowLockId").is_some()
                    || arguments.get("lock_id").is_some()
                    || arguments.get("lockId").is_some()
            });
        let flow_binding = self.flow_lock_binding_from_arguments(arguments)?;
        let agent_command = agent_command_from_arguments(arguments)?;
        let nodes = match flow_binding.as_ref() {
            Some((lock_id, _)) => self.locked_flow_node_specs(lock_id)?,
            None => node_specs(arguments)?,
        };
        validate_start_run_preconditions(self.state.runtime(), run_id, &nodes)?;

        if review_required {
            let Some((lock_id, content_hash)) = flow_binding.as_ref() else {
                return Ok(ToolCallResult::error(json!({
                    "ok": false,
                    "run_id": run_id,
                    "error": "flow_lock_id is required for review",
                    "next_tool": "prepare_flow_review"
                })));
            };
            match self.review_status_for_lock(lock_id) {
                Some(FlowReviewStatus::Approved | FlowReviewStatus::Bypassed) => {}
                Some(FlowReviewStatus::Rejected) => {
                    return Ok(ToolCallResult::error(json!({
                        "ok": false,
                        "run_id": run_id,
                        "flow_lock_id": lock_id,
                        "content_hash": content_hash,
                        "review_status": "rejected",
                        "error": "flow review rejected",
                        "next_tool": "prepare_flow_review",
                        "after_next_tool": "approve_flow_review"
                    })));
                }
                Some(FlowReviewStatus::Pending) | None => {
                    return Ok(ToolCallResult::error(json!({
                        "ok": false,
                        "run_id": run_id,
                        "flow_lock_id": lock_id,
                        "content_hash": content_hash,
                        "review_status": self.review_status_for_lock(lock_id).map(FlowReviewStatus::as_str).unwrap_or("missing"),
                        "error": "flow review required",
                        "next_tool": "prepare_flow_review",
                        "after_next_tool": "approve_flow_review"
                    })));
                }
            }
        }

        let expected_activation_ids = nodes
            .iter()
            .map(|node| node.id().to_string())
            .collect::<Vec<_>>();
        let tmux = self.start_run_tmux_metadata(run_id, arguments, &expected_activation_ids)?;
        let (activation_ids, report) = if let Some((lock_id, content_hash)) = flow_binding.as_ref()
        {
            self.state.tick_control(ControlCommand::StartRun {
                run_id: run_id.to_string(),
                nodes: Vec::new(),
            });
            self.state
                .runtime_mut()
                .apply_flow_lock(
                    run_id,
                    runtime::FlowLockMode::FutureActivations,
                    lock_id,
                    content_hash,
                )
                .map_err(ToolError::from_runtime)?;
            let activation_ids = nodes
                .iter()
                .map(|node| {
                    self.state
                        .runtime_mut()
                        .activate_node(run_id, node, None)
                        .map_err(ToolError::from_runtime)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let report = self.state.tick_control(ControlCommand::ResumeRun {
                run_id: run_id.to_string(),
            });
            (activation_ids, report)
        } else {
            let activation_ids = nodes
                .iter()
                .map(|node| node.id().to_string())
                .collect::<Vec<_>>();
            let report = self.state.tick_control(ControlCommand::StartRun {
                run_id: run_id.to_string(),
                nodes,
            });
            (activation_ids, report)
        };
        self.state
            .remember_tmux_allocation(run_id, &tmux.window, &tmux.panes);
        if let Some(command) = agent_command {
            self.state
                .run_agent_commands
                .insert(run_id.to_string(), command);
        }
        if let Some((lock_id, content_hash)) = flow_binding.as_ref() {
            let review_status = self.run_review_status_name(lock_id, review_required);
            self.remember_run_archive(run_id, lock_id, content_hash, review_status);
        }
        let actuation = self.actuate_locked_flow(run_id, &activation_ids)?;

        let run_status = self.run_status_string(run_id)?;
        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "activation_ids": activation_ids,
            "run_status": run_status,
            "tmux": tmux.structured,
            "actuation": {
                "sent": actuation.sent
            },
            "actuation_warnings": actuation.warnings,
            "flow_lock_id": flow_binding.as_ref().map(|(lock_id, _)| lock_id.as_str()),
            "content_hash": flow_binding.as_ref().map(|(_, content_hash)| content_hash.as_str()),
            "pipeline": report.pipeline
        })))
    }

    fn locked_flow_node_specs(&self, lock_id: &str) -> Result<Vec<runtime::NodeSpec>, ToolError> {
        let Some(lock) = self.state.flow_locks.get(lock_id) else {
            return Err(ToolError::invalid("flow lock not found"));
        };
        Ok(node_specs_from_locked_draft(lock.draft()))
    }

    pub(super) fn run_status(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let context = self.context_for_run(run_id)?;
        let run_status = context
            .get("run_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "run_status": run_status,
            "context": context
        })))
    }

    pub(super) fn run_why(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let context = self.context_for_run(run_id)?;
        let run_status = context
            .get("run_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let cause = concise_run_cause(&context, run_status);

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "run_status": run_status,
            "cause": cause
        })))
    }

    pub(super) fn pause_run(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        self.control_run(arguments, "pause_run", |run_id| ControlCommand::PauseRun {
            run_id: run_id.to_string(),
        })
    }

    pub(super) fn resume_run(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        self.control_run(arguments, "resume_run", |run_id| {
            ControlCommand::ResumeRun {
                run_id: run_id.to_string(),
            }
        })
    }

    pub(super) fn stop_run(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        self.control_run(arguments, "stop_run", |run_id| ControlCommand::StopRun {
            run_id: run_id.to_string(),
        })
    }

    pub(super) fn observe_stop(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let activation_id = require_string(arguments, &["activation_id", "activationId"])?;
        let reason = require_string(arguments, &["reason"])?;
        if !self.state.runtime().has_run(run_id) {
            return Ok(ToolCallResult::error(run_not_found_guidance(run_id)));
        }
        if !self
            .state
            .runtime()
            .state()
            .activations
            .contains_key(&(run_id.to_string(), activation_id.to_string()))
        {
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "activation_id": activation_id,
                "missing": ["activation"],
                "error": format!("activation not found in run {run_id}: {activation_id}")
            })));
        }

        let report = self.state.tick_stop_observation(
            run_id,
            activation_id,
            runtime::StopObservation::new(reason),
        );
        let tmux_allocations = self.allocate_missing_tmux_panes(run_id)?;
        self.actuate_locked_flow(run_id, &[])?;
        let tmux_cleanup = self.cleanup_tmux_pane_after_stop(run_id, activation_id, &report)?;
        let run_status = self.run_status_string(run_id)?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "activation_id": activation_id,
            "run_status": run_status,
            "stop_decisions": stop_decisions_json(activation_id, &report.stop_decisions),
            "tmux_allocations": tmux_allocations,
            "tmux_cleanup": tmux_cleanup,
            "pipeline": report.pipeline
        })))
    }

    fn control_run<F>(
        &mut self,
        arguments: &Value,
        control: &'static str,
        command: F,
    ) -> Result<ToolCallResult, ToolError>
    where
        F: FnOnce(&str) -> ControlCommand,
    {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        if !self.state.runtime().has_run(run_id) {
            return Ok(ToolCallResult::error(run_not_found_guidance(run_id)));
        }
        let report = self.state.tick_control(command(run_id));
        let tmux_allocations = self.allocate_missing_tmux_panes(run_id)?;
        self.actuate_locked_flow(run_id, &[])?;
        let run_status = self.run_status_string(run_id)?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "control": control,
            "run_status": run_status,
            "tmux_allocations": tmux_allocations,
            "pipeline": report.pipeline
        })))
    }

    pub(super) fn flow_lock_binding_from_arguments(
        &mut self,
        arguments: &Value,
    ) -> Result<Option<(String, String)>, ToolError> {
        if arguments.get("flow").is_some() {
            return self
                .lock_flow_from_arguments(arguments)
                .map(|(lock_id, content_hash)| Some((lock_id, content_hash)));
        }
        let Some(lock_id) = optional_string(
            arguments,
            &["flow_lock_id", "flowLockId", "lock_id", "lockId"],
        )?
        else {
            return Ok(None);
        };
        self.validate_flow_lock_binding(arguments, lock_id)
            .map(|content_hash| Some((lock_id.to_string(), content_hash)))
    }

    pub(super) fn require_flow_lock_binding_from_arguments(
        &mut self,
        arguments: &Value,
    ) -> Result<(String, String), ToolError> {
        self.flow_lock_binding_from_arguments(arguments)?
            .ok_or_else(|| ToolError::missing("flow_lock_id"))
    }

    fn lock_flow_from_arguments(
        &mut self,
        arguments: &Value,
    ) -> Result<(String, String), ToolError> {
        let draft = flow_draft_arg(arguments)?;
        let mode = flow_check_mode_arg(arguments)?;
        match flow::flow_lock(&draft, mode) {
            Ok(lock) => {
                let lock_id = lock.id().to_string();
                let content_hash = content_hash(lock.normalized_content());
                self.state.flow_locks.insert(lock_id.clone(), lock);
                Ok((lock_id, content_hash))
            }
            Err(err) => Err(ToolError::invalid(format!(
                "flow lock failed: {}",
                diagnostic_codes_text(&err.diagnostics)
            ))),
        }
    }

    fn validate_flow_lock_binding(
        &self,
        arguments: &Value,
        lock_id: &str,
    ) -> Result<String, ToolError> {
        let Some(lock) = self.state.flow_locks.get(lock_id) else {
            return Err(ToolError::invalid("flow lock not found"));
        };
        let expected_content_hash = content_hash(lock.normalized_content());
        if let Some(provided_content_hash) =
            optional_string(arguments, &["content_hash", "contentHash"])?
        {
            if provided_content_hash != expected_content_hash {
                return Err(ToolError::invalid("flow lock content hash mismatch"));
            }
        }
        Ok(expected_content_hash)
    }

    pub(super) fn review_status_for_lock(&self, lock_id: &str) -> Option<FlowReviewStatus> {
        self.state
            .flow_review_index
            .get(lock_id)
            .and_then(|review_id| self.state.reviews.get(review_id))
            .map(|record| record.status)
    }

    fn context_for_run(&self, run_id: &str) -> Result<Value, ToolError> {
        if !self.state.runtime().has_run(run_id) {
            return Err(ToolError::from_runtime(
                runtime::RuntimeError::RunNotFound {
                    run_id: run_id.to_string(),
                },
            ));
        }
        let snapshot = self.state.runtime_snapshot();
        let run = snapshot
            .run(run_id)
            .expect("checked run should be present in view snapshot");
        serde_json::to_value(run)
            .map_err(|_| ToolError::invalid("run context serialization failed"))
    }

    fn run_status_string(&self, run_id: &str) -> Result<String, ToolError> {
        let context = self.context_for_run(run_id)?;
        Ok(context
            .get("run_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string())
    }

    pub(super) fn remember_run_archive(
        &mut self,
        run_id: &str,
        lock_id: &str,
        content_hash: &str,
        review_status: String,
    ) {
        let flow_export_document = self
            .state
            .flow_locks
            .get(lock_id)
            .map(|lock| flow::flow_export(lock, flow::FlowExportFormat::Json))
            .unwrap_or_default();
        self.state.run_archives.insert(
            run_id.to_string(),
            RunArchive {
                flow_lock_id: lock_id.to_string(),
                content_hash: content_hash.to_string(),
                review_status,
                flow_export_document,
            },
        );
    }

    pub(super) fn run_review_status_name(&self, lock_id: &str, review_required: bool) -> String {
        match self.review_status_for_lock(lock_id) {
            Some(status) => status.as_str().to_string(),
            None if review_required => "missing".to_string(),
            None => "not_required".to_string(),
        }
    }

    fn actuate_locked_flow(
        &mut self,
        run_id: &str,
        activation_ids: &[String],
    ) -> Result<RuntimeActuation, ToolError> {
        let Some(lock_id) = self
            .state
            .runtime()
            .state()
            .flow_lock_id_by_run
            .get(run_id)
            .cloned()
        else {
            return Ok(RuntimeActuation::default());
        };
        let Some(lock) = self.state.flow_locks.get(&lock_id).cloned() else {
            return Ok(RuntimeActuation::default());
        };
        let draft = lock.draft().clone();
        let node_by_id = draft
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect::<BTreeMap<_, _>>();
        let selected_activation_ids = if activation_ids.is_empty() {
            self.state
                .runtime()
                .state()
                .activations
                .values()
                .filter(|activation| activation.run_id == run_id)
                .map(|activation| activation.activation_id.clone())
                .collect::<Vec<_>>()
        } else {
            activation_ids.to_vec()
        };
        let mut actuation = RuntimeActuation::default();

        for activation_id in selected_activation_ids {
            let key = (run_id.to_string(), activation_id.clone());
            if self.state.actuated_activations.contains(&key) {
                continue;
            }
            let Some(activation) = self.state.runtime().state().activations.get(&key).cloned()
            else {
                continue;
            };
            if activation.status != runtime::ActivationStatus::Running {
                continue;
            }
            let Some(node) = node_by_id.get(activation.node_id.as_str()) else {
                continue;
            };
            let Some(action) = node.action.as_ref() else {
                continue;
            };
            if action.driver != flow::NodeDriver::Agent {
                self.push_actuation_warning(
                    run_id,
                    json!({
                        "activation_id": activation.activation_id,
                        "node_id": activation.node_id,
                        "driver": node_driver_name(action.driver),
                        "message": "action driver is not supported for autonomous tmux actuation"
                    }),
                    &mut actuation,
                );
                continue;
            }
            let Some(agent_command) = self.state.run_agent_commands.get(run_id).cloned() else {
                self.push_actuation_warning(
                    run_id,
                    json!({
                        "activation_id": activation.activation_id,
                        "node_id": activation.node_id,
                        "driver": "agent",
                        "message": "tmux.agent_command is required before autonomous agent actuation"
                    }),
                    &mut actuation,
                );
                continue;
            };

            let Some(window) = self.state.tmux_windows.get(run_id).cloned() else {
                self.push_actuation_warning(
                    run_id,
                    json!({
                        "activation_id": activation.activation_id,
                        "node_id": activation.node_id,
                        "driver": "agent",
                        "message": "tmux pane mapping is required for agent actuation"
                    }),
                    &mut actuation,
                );
                continue;
            };
            let Some(pane) = self
                .state
                .tmux_panes
                .get(&(run_id.to_string(), activation.activation_id.clone()))
                .cloned()
            else {
                self.push_actuation_warning(
                    run_id,
                    json!({
                        "activation_id": activation.activation_id,
                        "node_id": activation.node_id,
                        "driver": "agent",
                        "message": "tmux pane mapping is required for agent actuation"
                    }),
                    &mut actuation,
                );
                continue;
            };

            let prompt = initial_agent_prompt(&draft, node, action);
            let metadata = TmuxActivationMetadata::new(
                pane.session_id(),
                run_id,
                window.name(),
                pane.window_id(),
                activation.activation_id.as_str(),
                pane.id(),
            );
            let mut launch_transaction_id = None;
            if !self.state.launched_activations.contains(&key) {
                match self
                    .tmux_adapter
                    .send_input_transaction(&metadata, &agent_command)
                {
                    Ok(transaction) => {
                        launch_transaction_id = Some(transaction.transaction_id().to_string());
                        self.record_machine_input(run_id, "agent_launch", transaction.record());
                        self.state.launched_activations.insert(key.clone());
                    }
                    Err(err) => {
                        self.push_actuation_warning(
                            run_id,
                            json!({
                                "activation_id": activation.activation_id,
                                "node_id": activation.node_id,
                                "driver": "agent",
                                "message": "tmux actuation failed before agent launch",
                                "error": err.to_string()
                            }),
                            &mut actuation,
                        );
                        continue;
                    }
                }
            }
            match self.tmux_adapter.send_input_transaction(&metadata, &prompt) {
                Ok(transaction) => {
                    let prompt_transaction_id = transaction.transaction_id().to_string();
                    self.record_machine_input(run_id, "node_prompt", transaction.record());
                    self.state.actuated_activations.insert(key);
                    actuation.sent.push(json!({
                        "activation_id": activation.activation_id,
                        "node_id": activation.node_id,
                        "driver": "agent",
                        "agent_command": agent_command,
                        "agent_launch_transaction_id": launch_transaction_id,
                        "prompt_transaction_id": prompt_transaction_id,
                        "pane_id": pane.id(),
                        "session_id": pane.session_id(),
                        "window_id": pane.window_id(),
                        "window_name": window.name()
                    }));
                }
                Err(err) => {
                    self.push_actuation_warning(
                        run_id,
                        json!({
                            "activation_id": activation.activation_id,
                            "node_id": activation.node_id,
                            "driver": "agent",
                            "message": "tmux actuation failed before prompt submission",
                            "error": err.to_string()
                        }),
                        &mut actuation,
                    );
                }
            }
        }

        Ok(actuation)
    }

    fn push_actuation_warning(
        &mut self,
        run_id: &str,
        warning: Value,
        actuation: &mut RuntimeActuation,
    ) {
        if !actuation.warnings.contains(&warning) {
            actuation.warnings.push(warning.clone());
        }
        let warnings = self
            .state
            .actuation_warnings
            .entry(run_id.to_string())
            .or_default();
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }

    fn record_machine_input(&mut self, run_id: &str, role: &str, record: &MachineInputRecord) {
        let mut value = serde_json::to_value(record).unwrap_or_else(|_| {
            json!({
                "transaction_id": record.transaction_id,
                "status": "unknown"
            })
        });
        if let Some(object) = value.as_object_mut() {
            object.insert("role".to_string(), json!(role));
        }
        self.state
            .machine_inputs
            .entry(run_id.to_string())
            .or_default()
            .push(value);
    }
}

#[derive(Debug, Default)]
struct RuntimeActuation {
    sent: Vec<Value>,
    warnings: Vec<Value>,
}

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

fn initial_agent_prompt(
    draft: &flow::FlowDraft,
    node: &flow::FlowNode,
    action: &flow::NodeAction,
) -> String {
    let resources_by_id = draft
        .resources
        .iter()
        .map(|resource| (resource.id.as_str(), resource))
        .collect::<BTreeMap<_, _>>();
    let mut prompt = action
        .prompt_ref
        .as_deref()
        .and_then(|prompt_ref| resources_by_id.get(prompt_ref))
        .map(|resource| resource_source_body(&resource.source).to_string())
        .filter(|body| !body.is_empty())
        .unwrap_or_else(|| format!("Run node {}.", node.id));

    let resources = action
        .resource_refs
        .iter()
        .filter_map(|resource_id| {
            resources_by_id
                .get(resource_id.as_str())
                .map(|resource| (resource_id, *resource))
        })
        .collect::<Vec<_>>();
    if !resources.is_empty() {
        prompt.push_str("\n\nResources:");
        for (resource_id, resource) in resources {
            prompt.push('\n');
            prompt.push_str(resource_id);
            prompt.push_str(" (");
            prompt.push_str(resource_kind_name(&resource.kind));
            prompt.push_str("): ");
            prompt.push_str(resource_source_body(&resource.source));
        }
    }

    prompt
}

fn resource_source_body(source: &str) -> &str {
    source.strip_prefix("inline:").unwrap_or(source).trim()
}

fn node_driver_name(driver: flow::NodeDriver) -> &'static str {
    match driver {
        flow::NodeDriver::Agent => "agent",
        flow::NodeDriver::Script => "script",
        flow::NodeDriver::Review => "review",
        flow::NodeDriver::Human => "human",
    }
}

fn resource_kind_name(kind: &flow::ResourceKind) -> &'static str {
    match kind {
        flow::ResourceKind::Schema => "schema",
        flow::ResourceKind::Rule => "rule",
        flow::ResourceKind::Profile => "profile",
        flow::ResourceKind::View => "view",
        flow::ResourceKind::Prompt => "prompt",
        flow::ResourceKind::Script => "script",
        flow::ResourceKind::Flow => "flow",
        flow::ResourceKind::Readme => "readme",
    }
}

fn node_specs_from_locked_draft(draft: &flow::FlowDraft) -> Vec<runtime::NodeSpec> {
    let route_targets = draft
        .routes
        .iter()
        .map(|route| route.activate.as_str())
        .collect::<HashSet<_>>();
    let node_contracts = flow::NodeContract::from_draft(draft);
    let initial_nodes = node_contracts
        .iter()
        .filter(|contract| !route_targets.contains(contract.node_id.as_str()))
        .map(node_spec_from_contract)
        .collect::<Vec<_>>();

    if initial_nodes.is_empty() {
        node_contracts.iter().map(node_spec_from_contract).collect()
    } else {
        initial_nodes
    }
}

fn node_spec_from_contract(contract: &flow::NodeContract) -> runtime::NodeSpec {
    let required_artifacts = contract
        .artifact_requirements
        .iter()
        .filter(|artifact| artifact.required)
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    let required_effects = contract
        .effect_requirements
        .iter()
        .filter(|effect| effect.required)
        .map(|effect| effect.id.clone())
        .collect::<Vec<_>>();

    runtime::NodeSpec::new(&contract.node_id).with_stop_contract(runtime::StopContract::new(
        required_artifacts,
        required_effects,
    ))
}

fn concise_run_cause(context: &Value, run_status: &str) -> &'static str {
    if context
        .get("missing_stop_contract_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        return "missing stop requirements";
    }
    match run_status {
        "pending_review" => "run is waiting for review",
        "ready" => "run is ready",
        "running" => "run is running",
        "paused" => "run is paused",
        "blocked" => "run is blocked",
        "quiescent" => "run is quiescent",
        "completed" => "run is completed",
        "failed" => "run has failed",
        "stopping" => "run is stopping",
        "stopped" => "run is stopped",
        _ => "run state is unknown",
    }
}

fn stop_decisions_json(activation_id: &str, decisions: &[runtime::StopDecision]) -> Vec<Value> {
    decisions
        .iter()
        .map(|decision| {
            let missing = decision
                .missing_artifacts
                .iter()
                .map(|artifact| format!("artifact:{artifact}"))
                .chain(
                    decision
                        .missing_effects
                        .iter()
                        .map(|effect| format!("effect:{effect}")),
                )
                .collect::<Vec<_>>();
            json!({
                "activation_id": activation_id,
                "decision": stop_decision_kind_name(decision.kind),
                "attempt": decision.attempt,
                "reason": decision.reason,
                "missing": missing
            })
        })
        .collect()
}

fn stop_decision_kind_name(kind: runtime::StopDecisionKind) -> &'static str {
    match kind {
        runtime::StopDecisionKind::Allow => "allow",
        runtime::StopDecisionKind::Deny => "deny",
        runtime::StopDecisionKind::Block => "block",
        runtime::StopDecisionKind::Yield => "yield",
    }
}
