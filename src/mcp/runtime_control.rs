use std::collections::HashSet;

use crate::adapters::tmux::CommandRunner;
use crate::flow;
use crate::runtime::{self, ControlCommand};
use serde_json::{Value, json};

use super::{
    FlowReviewStatus, McpServer, ToolCallResult, ToolError, content_hash, diagnostic_codes_text,
    flow_check_mode_arg, flow_draft_arg, node_specs, optional_bool, optional_string,
    require_string, run_not_found_guidance, validate_start_run_preconditions,
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

        let run_status = self.run_status_string(run_id)?;
        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "activation_ids": activation_ids,
            "run_status": run_status,
            "tmux": tmux.structured,
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
