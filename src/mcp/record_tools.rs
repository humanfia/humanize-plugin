use crate::adapters::tmux::CommandRunner;
use crate::flow;
use crate::run_assets::{HookFactInput, TopologyDecisionInput};
use crate::runtime;
use serde_json::{Value, json};

use super::{
    McpServer, ToolCallResult, ToolError, optional_string, require_string, run_not_found_guidance,
};

const HOOK_ID_MAX_BYTES: usize = 128;
const SOURCE_NATIVE_ID_MAX_BYTES: usize = 256;
const HOOK_PAYLOAD_MAX_BYTES: usize = 64 * 1024;

impl<R: CommandRunner> McpServer<R> {
    pub(super) fn record_hook_fact(
        &mut self,
        arguments: &Value,
    ) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        if !self.state.runtime().has_run(run_id) {
            return Ok(ToolCallResult::error(run_not_found_guidance(run_id)));
        }
        self.ensure_run_asset_manifest(run_id)?;
        let session_id = require_string(arguments, &["session_id", "sessionId"])?;
        validate_bounded_id("session_id", session_id, HOOK_ID_MAX_BYTES)?;
        let hook = require_string(arguments, &["hook"])?;
        validate_hook_name(hook)?;
        let source_native_id = optional_string(arguments, &["source_native_id", "sourceNativeId"])?
            .map(str::to_string)
            .unwrap_or_else(|| format!("hook:{session_id}:{hook}"));
        validate_bounded_id(
            "source_native_id",
            &source_native_id,
            SOURCE_NATIVE_ID_MAX_BYTES,
        )?;
        let activation_id =
            optional_string(arguments, &["activation_id", "activationId"])?.map(str::to_string);
        if let Some(activation_id) = activation_id.as_deref() {
            validate_bounded_id("activation_id", activation_id, HOOK_ID_MAX_BYTES)?;
            let state = self.state.runtime().state();
            if !state
                .activations
                .contains_key(&(run_id.to_string(), activation_id.to_string()))
            {
                return Err(ToolError::invalid(format!(
                    "activation not found for run_id {run_id}: {activation_id}"
                )));
            }
        }
        let payload = arguments.get("payload").cloned().unwrap_or(Value::Null);
        let payload_size = serde_json::to_vec(&payload)
            .map_err(|err| ToolError::invalid(format!("payload serialization failed: {err}")))?
            .len();
        if payload_size > HOOK_PAYLOAD_MAX_BYTES {
            return Err(ToolError::invalid(format!(
                "payload exceeds {HOOK_PAYLOAD_MAX_BYTES} bytes"
            )));
        }
        let causal_id = optional_string(arguments, &["causal_id", "causalId"])?.map(str::to_string);
        if let Some(causal_id) = causal_id.as_deref() {
            validate_bounded_id("causal_id", causal_id, SOURCE_NATIVE_ID_MAX_BYTES)?;
        }
        let correlation_id =
            optional_string(arguments, &["correlation_id", "correlationId"])?.map(str::to_string);
        if let Some(correlation_id) = correlation_id.as_deref() {
            validate_bounded_id("correlation_id", correlation_id, SOURCE_NATIVE_ID_MAX_BYTES)?;
        }
        let manifest = self
            .state
            .run_assets
            .get_mut(run_id)
            .ok_or_else(|| ToolError::invalid("run asset manifest not found"))?;
        let context_generation = self
            .run_asset_store
            .record_hook_fact(
                manifest,
                HookFactInput {
                    session_id: session_id.to_string(),
                    activation_id,
                    hook: hook.to_string(),
                    source_native_id,
                    payload,
                    causal_id,
                    correlation_id,
                },
            )
            .map_err(ToolError::from_run_asset)?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "session_id": session_id,
            "hook": hook,
            "context_generation": context_generation,
            "run_assets": self.run_assets_json(run_id)
        })))
    }

    pub(in crate::mcp) fn record_new_runtime_events(
        &mut self,
        run_id: &str,
    ) -> Result<(), ToolError> {
        if !self.state.run_assets.contains_key(run_id) {
            return Ok(());
        }
        let last_sequence = self
            .state
            .recorded_runtime_sequences
            .get(run_id)
            .copied()
            .unwrap_or(0);
        let events = self
            .state
            .runtime()
            .events()
            .iter()
            .filter(|event| event.sequence > last_sequence)
            .filter(|event| event.source.run_id.as_deref() == Some(run_id))
            .cloned()
            .collect::<Vec<_>>();
        let mut latest_sequence = last_sequence;
        for event in events {
            let manifest = self
                .state
                .run_assets
                .get_mut(run_id)
                .ok_or_else(|| ToolError::invalid("run asset manifest not found"))?;
            self.run_asset_store
                .record_runtime_event(manifest, &event)
                .map_err(ToolError::from_run_asset)?;
            latest_sequence = latest_sequence.max(event.sequence);
        }
        self.state
            .recorded_runtime_sequences
            .insert(run_id.to_string(), latest_sequence);
        Ok(())
    }

    pub(in crate::mcp) fn record_explicit_fanout_decision(
        &mut self,
        run_id: &str,
        node_id: &str,
        artifact_key: &str,
        activation_ids: &[String],
    ) -> Result<(), ToolError> {
        if !self.state.run_assets.contains_key(run_id) {
            return Ok(());
        }
        let source_artifact = self.source_artifact_json(run_id, artifact_key);
        let causal_id = source_artifact
            .as_ref()
            .and_then(|artifact| artifact["artifact_id"].as_str())
            .map(str::to_string);
        let manifest = self
            .state
            .run_assets
            .get_mut(run_id)
            .ok_or_else(|| ToolError::invalid("run asset manifest not found"))?;
        self.run_asset_store
            .record_topology_decision(
                manifest,
                TopologyDecisionInput {
                    source: "mcp",
                    source_native_id: format!("fanout:{node_id}:{artifact_key}"),
                    fact: json!({
                        "decision": "fanout_from_artifact",
                        "node_id": node_id,
                        "source_artifact": source_artifact,
                        "planned_activation_ids": activation_ids,
                        "applied_activation_ids": activation_ids,
                    }),
                    causal_id,
                    correlation_id: None,
                },
            )
            .map_err(ToolError::from_run_asset)
    }

    pub(in crate::mcp) fn record_route_topology_decisions(
        &mut self,
        report: &runtime::DriverTickReport,
    ) -> Result<(), ToolError> {
        for decision in &report.route_decisions {
            if !self.state.run_assets.contains_key(&decision.run_id) {
                continue;
            }
            let source_artifact = route_source_artifact_json(decision);
            let causal_id = decision
                .source_artifact
                .as_ref()
                .and_then(|artifact| artifact.artifact_id.clone());
            let manifest = self
                .state
                .run_assets
                .get_mut(&decision.run_id)
                .ok_or_else(|| ToolError::invalid("run asset manifest not found"))?;
            self.run_asset_store
                .record_topology_decision(
                    manifest,
                    TopologyDecisionInput {
                        source: "runtime_driver",
                        source_native_id: format!("route:{}", decision.route_id),
                        fact: json!({
                            "decision": "route_applied",
                            "route": {
                                "route_index": decision.route_index,
                                "route_id": decision.route_id,
                                "flow_lock_id": decision.flow_lock_id,
                                "predicate": decision.predicate,
                                "for_each": decision.for_each,
                            },
                            "source_artifact": source_artifact,
                            "planned_activation_ids": decision.planned_activation_ids,
                            "applied_activation_ids": decision.applied_activation_ids,
                        }),
                        causal_id,
                        correlation_id: Some(decision.flow_lock_id.clone()),
                    },
                )
                .map_err(ToolError::from_run_asset)?;
        }
        Ok(())
    }

    pub(in crate::mcp) fn record_run_qos_intent(
        &mut self,
        run_id: &str,
        qos: &flow::FlowQosIntent,
    ) -> Result<(), ToolError> {
        if qos.is_default() {
            return Ok(());
        }
        let manifest = self
            .state
            .run_assets
            .get_mut(run_id)
            .ok_or_else(|| ToolError::invalid("run asset manifest not found"))?;
        self.run_asset_store
            .record_qos_intent(manifest, qos)
            .map_err(ToolError::from_run_asset)
    }

    fn source_artifact_json(&self, run_id: &str, artifact_key: &str) -> Option<Value> {
        let state = self.state.runtime().state();
        let artifact_id = state
            .latest_artifact_by_slot_index
            .get(&(run_id.to_string(), artifact_key.to_string()))?;
        Some(json!({
            "key": artifact_key,
            "artifact_id": artifact_id,
        }))
    }
}

fn validate_bounded_id(name: &str, value: &str, max_bytes: usize) -> Result<(), ToolError> {
    if value.trim().is_empty() {
        return Err(ToolError::invalid(format!("{name} must be non-empty")));
    }
    if value.len() > max_bytes {
        return Err(ToolError::invalid(format!(
            "{name} must be at most {max_bytes} bytes"
        )));
    }
    Ok(())
}

fn validate_hook_name(hook: &str) -> Result<(), ToolError> {
    validate_bounded_id("hook", hook, HOOK_ID_MAX_BYTES)?;
    if matches!(hook, "compaction_pending" | "compaction_finished") || is_namespaced_hook(hook) {
        return Ok(());
    }
    Err(ToolError::invalid(
        "hook must be a documented hook name or namespaced extension",
    ))
}

fn is_namespaced_hook(hook: &str) -> bool {
    let Some((namespace, name)) = hook.split_once('.') else {
        return false;
    };
    !namespace.is_empty()
        && !name.is_empty()
        && hook.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}

fn route_source_artifact_json(decision: &runtime::RouteDecision) -> Option<Value> {
    decision.source_artifact.as_ref().map(|artifact| {
        json!({
            "key": artifact.key,
            "artifact_id": artifact.artifact_id,
        })
    })
}
