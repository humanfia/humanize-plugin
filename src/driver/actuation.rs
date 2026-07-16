use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::adapters::tmux::TmuxActivationMetadata;
use crate::flow;
use crate::input_ledger::{
    MachineInputRecord, MachineInputStatus, machine_input_payload_hash,
    normalize_machine_input_text,
};
use crate::run_assets::machine_input_source_native_id;
use crate::runtime;

use super::delivery::{DELIVERY_ROLE_AGENT_LAUNCH, DELIVERY_ROLE_NODE_PROMPT};
use super::storage::read_jsonl_recover_torn_tail;
use super::{DriverFailure, RuntimeDriverService};

const AGENT_LAUNCH_LEDGER_PROJECTION: &str = "participant-agent-launch";

#[derive(Debug, Clone, Default)]
pub(super) struct DriverActuation {
    sent: Vec<Value>,
    warnings: Vec<Value>,
}

impl DriverActuation {
    pub(super) fn with_warnings(warnings: Vec<Value>) -> Self {
        Self {
            sent: Vec::new(),
            warnings,
        }
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "sent": self.sent,
            "warnings": self.warnings
        })
    }

    pub(super) fn requires_pause(&self) -> bool {
        self.warnings
            .iter()
            .any(|warning| warning.get("pause_required").and_then(Value::as_bool) != Some(false))
    }
}

impl RuntimeDriverService {
    pub(super) fn actuate_activations(
        &mut self,
        activation_ids: &[String],
    ) -> Result<DriverActuation, DriverFailure> {
        let mut actuation = DriverActuation::default();
        if activation_ids.is_empty() {
            return Ok(actuation);
        }
        let Some(tmux) = self.tmux.clone() else {
            return Ok(actuation);
        };
        let Some(lock_id) = self
            .driver
            .runtime()
            .state()
            .flow_lock_id_by_run
            .get(&self.config.run_id)
            .cloned()
        else {
            return Ok(actuation);
        };
        let Some(lock) = self
            .locks
            .get(&lock_id)
            .and_then(|package| package.lock().ok())
        else {
            return Ok(actuation);
        };
        let draft = lock.draft().clone();
        let node_by_id = draft
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect::<BTreeMap<_, _>>();

        for activation_id in activation_ids {
            let key = (self.config.run_id.clone(), activation_id.clone());
            let Some(activation) = self.driver.runtime().state().activations.get(&key).cloned()
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
            let Some(pane) = tmux.panes.get(activation_id).cloned() else {
                actuation.warnings.push(json!({
                    "activation_id": activation.activation_id,
                    "node_id": activation.node_id,
                    "driver": node_driver_name(action.driver),
                    "message": "tmux pane mapping is required for agent actuation"
                }));
                continue;
            };
            let allocation_key = (activation_id.clone(), pane.allocation_generation);
            if self.settled_actuation_activations.contains(&allocation_key) {
                continue;
            }
            if action.driver == flow::NodeDriver::Human {
                self.settled_actuation_activations.insert(allocation_key);
                continue;
            }
            if !is_autonomous_agent_backed_driver(action.driver) {
                actuation.warnings.push(json!({
                    "activation_id": activation.activation_id,
                    "node_id": activation.node_id,
                    "driver": node_driver_name(action.driver),
                    "message": "action driver is not supported for autonomous tmux actuation"
                }));
                continue;
            }
            let readiness_nonce = self.ensure_activation_capture_started(
                &activation,
                node_driver_name(action.driver),
                &pane,
            )?;
            let participant = self.ensure_participant_started(
                &activation.activation_id,
                &pane,
                &readiness_nonce,
            )?;
            let participant_binding_path = self.participant_binding_path(&participant)?;
            let launch_command =
                agent_launch_command(&tmux.agent_command, &participant_binding_path)?;
            let metadata = TmuxActivationMetadata::new(
                pane.session_id.as_str(),
                self.config.run_id.as_str(),
                pane.window_name.as_str(),
                pane.window_id.as_str(),
                activation.activation_id.as_str(),
                pane.pane_id.as_str(),
            )
            .with_allocation_generation(pane.allocation_generation);
            let mut launch_transaction_id = None;
            if !self
                .agent_launch_submitted_activations
                .contains(&(activation_id.clone(), pane.allocation_generation))
                && let Some(delivery) = self.ambiguous_input_delivery(
                    &activation.activation_id,
                    DELIVERY_ROLE_AGENT_LAUNCH,
                    pane.allocation_generation,
                )
            {
                if delivery.pane_id == pane.pane_id
                    && let Some(record) = self.submitted_machine_input(
                        &activation.activation_id,
                        &delivery.pane_id,
                        &launch_command,
                        AGENT_LAUNCH_LEDGER_PROJECTION,
                    )?
                {
                    launch_transaction_id = Some(record.transaction_id.clone());
                    self.finish_input_delivery(
                        &activation.activation_id,
                        DELIVERY_ROLE_AGENT_LAUNCH,
                        None,
                        pane.allocation_generation,
                        json!({
                            "pane_id": delivery.pane_id,
                            "transaction_id": record.transaction_id,
                            "recovered": true
                        }),
                    )?;
                }
                if !self
                    .agent_launch_submitted_activations
                    .contains(&(activation_id.clone(), pane.allocation_generation))
                {
                    actuation.warnings.push(self.ambiguous_delivery_warning(
                        &delivery,
                        &activation.node_id,
                        node_driver_name(action.driver),
                    ));
                    continue;
                }
            }
            if !self
                .agent_launch_submitted_activations
                .contains(&(activation_id.clone(), pane.allocation_generation))
            {
                let delivery = self.start_input_delivery(
                    &activation.activation_id,
                    &pane.pane_id,
                    DELIVERY_ROLE_AGENT_LAUNCH,
                    &launch_command,
                )?;
                match self.tmux_adapter.send_input_transaction_with_projection(
                    &metadata,
                    &launch_command,
                    AGENT_LAUNCH_LEDGER_PROJECTION,
                ) {
                    Ok(transaction) => {
                        launch_transaction_id = Some(transaction.transaction_id().to_string());
                        self.record_machine_input(
                            DELIVERY_ROLE_AGENT_LAUNCH,
                            transaction.record(),
                        )?;
                        self.finish_input_delivery(
                            &activation.activation_id,
                            DELIVERY_ROLE_AGENT_LAUNCH,
                            None,
                            pane.allocation_generation,
                            json!({
                                "pane_id": pane.pane_id,
                                "transaction_id": launch_transaction_id
                            }),
                        )?;
                    }
                    Err(err) => {
                        let mut warning = self.ambiguous_delivery_warning(
                            &delivery,
                            &activation.node_id,
                            node_driver_name(action.driver),
                        );
                        warning["error"] = Value::String(err.to_string());
                        actuation.warnings.push(warning);
                        continue;
                    }
                }
            }
            let prompt = initial_agent_prompt(&draft, node, action);
            if let Some(delivery) = self.ambiguous_input_delivery(
                &activation.activation_id,
                DELIVERY_ROLE_NODE_PROMPT,
                pane.allocation_generation,
            ) {
                if delivery.pane_id == pane.pane_id
                    && let Some(record) = self.submitted_machine_input(
                        &activation.activation_id,
                        &delivery.pane_id,
                        &prompt,
                        &prompt,
                    )?
                {
                    self.finish_input_delivery(
                        &activation.activation_id,
                        DELIVERY_ROLE_NODE_PROMPT,
                        None,
                        pane.allocation_generation,
                        json!({
                            "pane_id": delivery.pane_id,
                            "transaction_id": record.transaction_id,
                            "recovered": true
                        }),
                    )?;
                    actuation.sent.push(json!({
                        "activation_id": activation.activation_id,
                        "node_id": activation.node_id,
                        "driver": node_driver_name(action.driver),
                        "agent_command": tmux.agent_command,
                        "agent_launch_transaction_id": launch_transaction_id,
                        "prompt_transaction_id": record.transaction_id,
                        "readiness": { "status": "submission_recovered" },
                        "pane_id": pane.pane_id,
                        "session_id": pane.session_id,
                        "window_id": pane.window_id,
                        "window_name": pane.window_name,
                        "recovered": true
                    }));
                    continue;
                }
                actuation.warnings.push(self.ambiguous_delivery_warning(
                    &delivery,
                    &activation.node_id,
                    node_driver_name(action.driver),
                ));
                continue;
            }
            let readiness = match self.wait_for_agent_readiness(
                &activation.activation_id,
                &pane.pane_id,
                pane.allocation_generation,
            )? {
                Ok(readiness) => readiness,
                Err(warning) => {
                    actuation.warnings.push(warning);
                    continue;
                }
            };
            let readiness = if let Some(pattern) = tmux.actuation.agent_ready_pattern.as_deref() {
                match self.tmux_adapter.wait_for_pane_text(
                    &metadata,
                    pattern,
                    std::time::Duration::from_millis(tmux.actuation.agent_ready_timeout_ms),
                ) {
                    Ok(()) => {
                        let mut readiness = readiness;
                        readiness["tmux_marker"] = Value::String("observed".to_string());
                        readiness
                    }
                    Err(err) => {
                        actuation.warnings.push(json!({
                            "activation_id": activation.activation_id,
                            "pane_id": pane.pane_id,
                            "allocation_generation": pane.allocation_generation,
                            "status": "readiness_pending",
                            "pause_required": false,
                            "message": "configured tmux readiness marker is pending",
                            "error": err.to_string()
                        }));
                        continue;
                    }
                }
            } else {
                match self.tmux_adapter.wait_for_inferred_agent_readiness(
                    &metadata,
                    &tmux.agent_command,
                    std::time::Duration::from_millis(tmux.actuation.agent_ready_timeout_ms),
                ) {
                    Ok(Some(profile)) => {
                        let mut readiness = readiness;
                        readiness["tmux_marker"] = Value::String("observed".to_string());
                        readiness["tmux_profile"] = Value::String(profile.to_string());
                        readiness
                    }
                    Ok(None) => readiness,
                    Err(err) => {
                        actuation.warnings.push(json!({
                            "activation_id": activation.activation_id,
                            "pane_id": pane.pane_id,
                            "allocation_generation": pane.allocation_generation,
                            "status": "readiness_pending",
                            "pause_required": false,
                            "message": "inferred tmux input surface is pending",
                            "error": err.to_string()
                        }));
                        continue;
                    }
                }
            };
            let delivery = self.start_input_delivery(
                &activation.activation_id,
                &pane.pane_id,
                DELIVERY_ROLE_NODE_PROMPT,
                &prompt,
            )?;
            match self
                .tmux_adapter
                .send_clean_input_transaction_with_agent_acceptance(
                    &metadata,
                    &prompt,
                    tmux.actuation.prompt_submit_key_count,
                    &tmux.agent_command,
                    std::time::Duration::from_millis(tmux.actuation.agent_ready_timeout_ms),
                ) {
                Ok(transaction) => {
                    let prompt_transaction_id = transaction.transaction_id().to_string();
                    let acceptance = transaction.acceptance().map(|acceptance| {
                        json!({
                            "profile": acceptance.profile(),
                            "signal": acceptance.signal()
                        })
                    });
                    self.record_machine_input(DELIVERY_ROLE_NODE_PROMPT, transaction.record())?;
                    self.finish_input_delivery(
                        &activation.activation_id,
                        DELIVERY_ROLE_NODE_PROMPT,
                        None,
                        pane.allocation_generation,
                        json!({
                            "pane_id": pane.pane_id,
                            "transaction_id": prompt_transaction_id,
                            "acceptance": acceptance
                        }),
                    )?;
                    actuation.sent.push(json!({
                        "activation_id": activation.activation_id,
                        "node_id": activation.node_id,
                        "driver": node_driver_name(action.driver),
                        "agent_command": tmux.agent_command,
                        "agent_launch_transaction_id": launch_transaction_id,
                        "prompt_transaction_id": prompt_transaction_id,
                        "prompt_submit_key_count": tmux.actuation.prompt_submit_key_count,
                        "readiness": readiness,
                        "pane_id": pane.pane_id,
                        "session_id": pane.session_id,
                        "window_id": pane.window_id,
                        "window_name": pane.window_name
                    }));
                }
                Err(err) => {
                    let mut warning = self.ambiguous_delivery_warning(
                        &delivery,
                        &activation.node_id,
                        node_driver_name(action.driver),
                    );
                    warning["error"] = Value::String(err.to_string());
                    actuation.warnings.push(warning);
                }
            }
        }
        Ok(actuation)
    }

    pub(super) fn released_delivery_barriers(&self, activation_ids: &[String]) -> Vec<Value> {
        let Some(tmux) = self.tmux.as_ref() else {
            return Vec::new();
        };
        let mut barriers = Vec::new();
        for activation_id in activation_ids {
            let Some(pane) = tmux.panes.get(activation_id) else {
                continue;
            };
            for role in [DELIVERY_ROLE_AGENT_LAUNCH, DELIVERY_ROLE_NODE_PROMPT] {
                if self
                    .ambiguous_input_delivery(activation_id, role, pane.allocation_generation)
                    .is_some()
                {
                    continue;
                }
                let Some(submission) =
                    self.submitted_deliveries
                        .get(&super::delivery::delivery_key(
                            activation_id,
                            role,
                            None,
                            pane.allocation_generation,
                        ))
                else {
                    continue;
                };
                barriers.push(json!({
                    "activation_id": submission.activation_id,
                    "pane_id": submission.pane_id,
                    "allocation_generation": submission.allocation_generation,
                    "role": submission.role,
                    "payload_hash": submission.payload_hash,
                    "started_event_sequence": submission.started_event_sequence,
                    "reason": "pane_released_after_submission"
                }));
            }
        }
        barriers
    }

    fn wait_for_agent_readiness(
        &self,
        activation_id: &str,
        pane_id: &str,
        allocation_generation: u64,
    ) -> Result<Result<Value, Value>, DriverFailure> {
        if let Some(ready) =
            self.participant_readiness(activation_id, pane_id, allocation_generation)
        {
            return Ok(Ok(ready));
        }
        Ok(Err(json!({
            "activation_id": activation_id,
            "pane_id": pane_id,
            "allocation_generation": allocation_generation,
            "status": "readiness_pending",
            "pause_required": false,
            "message": "participant SessionStart binding is pending"
        })))
    }

    pub(super) fn record_machine_input(
        &mut self,
        role: &str,
        record: &crate::input_ledger::MachineInputRecord,
    ) -> Result<(), DriverFailure> {
        let source_native_id = machine_input_source_native_id(&record.transaction_id);
        if self.published_record_sources.contains(&source_native_id) {
            return Ok(());
        }
        let mut manifest = self.load_run_asset_manifest()?;
        self.run_asset_store
            .record_machine_input(&mut manifest, role, record)
            .map_err(DriverFailure::from_run_asset)?;
        self.published_record_sources.insert(source_native_id);
        Ok(())
    }

    fn submitted_machine_input(
        &self,
        activation_id: &str,
        pane_id: &str,
        text: &str,
        projection: &str,
    ) -> Result<Option<MachineInputRecord>, DriverFailure> {
        let path = self.driver_dir().join("machine-inputs.jsonl");
        let records = read_jsonl_recover_torn_tail::<MachineInputRecord>(&path)
            .map_err(|err| DriverFailure::new("run_asset_error", err.to_string()))?;
        let normalized_text = normalize_machine_input_text(projection);
        let payload_hash = machine_input_payload_hash(text);
        Ok(records.into_iter().rev().find(|record| {
            record.run_id == self.config.run_id
                && record.activation_id == activation_id
                && record.pane_id == pane_id
                && record.allocation_generation
                    == self
                        .tmux
                        .as_ref()
                        .and_then(|tmux| tmux.panes.get(activation_id))
                        .map(|pane| pane.allocation_generation)
                        .unwrap_or(0)
                && record.normalized_text == normalized_text
                && record.payload_hash == payload_hash
                && record.status == MachineInputStatus::Submitted
        }))
    }
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
        .map(|resource| resource.source.clone())
        .filter(|body| !body.is_empty())
        .unwrap_or_else(|| "Complete the assigned task.".to_string());

    let resources = action
        .resource_refs
        .iter()
        .filter_map(|resource_id| {
            resources_by_id
                .get(resource_id.as_str())
                .map(|resource| resource.source.as_str())
        })
        .filter(|body| !body.is_empty())
        .collect::<Vec<_>>();
    if !resources.is_empty() {
        prompt.push_str("\n\nResources:");
        for body in resources {
            prompt.push_str("\n- ");
            prompt.push_str(body);
        }
    }

    if let Some(contract) = flow::NodeContract::from_draft(draft)
        .into_iter()
        .find(|contract| contract.node_id == node.id)
    {
        let artifacts = contract
            .artifact_requirements
            .into_iter()
            .filter(|requirement| requirement.required)
            .map(|requirement| requirement.id)
            .collect::<Vec<_>>();
        let effects = contract
            .effect_requirements
            .into_iter()
            .filter(|requirement| requirement.required)
            .map(|requirement| requirement.id)
            .collect::<Vec<_>>();
        if !artifacts.is_empty() || !effects.is_empty() {
            prompt.push_str("\n\nRequired outputs:");
            for artifact in artifacts {
                prompt.push_str("\n- Artifact: ");
                prompt.push_str(&artifact);
            }
            for effect in effects {
                prompt.push_str("\n- Effect: ");
                prompt.push_str(&effect);
            }
        }
    }

    let profile = flow::flow_node_work_profile(node);
    prompt.push_str("\n\nExecution boundaries:");
    prompt.push_str("\n- Workspace access: ");
    prompt.push_str(profile.workspace_access.as_str());
    prompt.push_str("\n- Tool execution: ");
    prompt.push_str(profile.tool_execution.as_str());
    prompt.push_str("\n- Network access: ");
    prompt.push_str(profile.network_access.as_str());
    for scope in flow::effective_node_write_scopes(&draft.policies, node) {
        prompt.push_str("\n- Write scope: ");
        prompt.push_str(write_scope_name(&scope));
    }
    prompt.push_str("\n\nDeliver required outputs through Humanize, then exit normally.");

    prompt
}

fn agent_launch_command(
    configured_command: &str,
    binding_file: &std::path::Path,
) -> Result<String, DriverFailure> {
    let hook_binary = participant_hook_binary()?;
    let wrapper = format!(
        "{configured_command}; status=$?; {} --participant-exited-hook --exit-status \"$status\"; exit \"$status\"",
        shell_single_quote(&hook_binary.to_string_lossy())
    );
    Ok(format!(
        "env HUMANIZE_PARTICIPANT_BINDING_FILE={} sh -c {}",
        shell_single_quote(&binding_file.to_string_lossy()),
        shell_single_quote(&wrapper)
    ))
}

fn participant_hook_binary() -> Result<std::path::PathBuf, DriverFailure> {
    let mut path = std::env::current_exe()
        .map_err(|err| DriverFailure::new("agent_launch_failed", err.to_string()))?;
    path.set_file_name("humanize-plugin-mcp");
    Ok(path)
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn is_autonomous_agent_backed_driver(driver: flow::NodeDriver) -> bool {
    matches!(driver, flow::NodeDriver::Agent | flow::NodeDriver::Review)
}

fn node_driver_name(driver: flow::NodeDriver) -> &'static str {
    match driver {
        flow::NodeDriver::Agent => "agent",
        flow::NodeDriver::Script => "script",
        flow::NodeDriver::Review => "review",
        flow::NodeDriver::Human => "human",
    }
}

fn write_scope_name(scope: &flow::WriteScope) -> &str {
    match scope {
        flow::WriteScope::Artifact(value) | flow::WriteScope::Resource(value) => value,
        flow::WriteScope::Workspace => "workspace",
        flow::WriteScope::System => "system",
    }
}
