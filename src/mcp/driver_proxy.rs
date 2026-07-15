use crate::adapters::tmux::CommandRunner;
use crate::driver::protocol::DriverWire;
use serde_json::{Map, Value, json};

use super::driver_cleanup::{
    persist_private_mcp_diagnostic, publish_cleanup_report, sanitize_public_driver_response,
};
use super::participant::McpCaller;
use super::{
    McpServer, McpServerState, ToolCallResult, ToolError, flow_lock_mode_arg, flow_lock_mode_name,
    node_spec_from_arguments, optional_flow_lock_mode_arg, optional_string, optional_u64,
    require_string,
};

pub(super) struct ToolArgumentContext<'a> {
    state: &'a McpServerState,
    caller: &'a McpCaller,
}

impl<'a> ToolArgumentContext<'a> {
    pub(super) fn new(state: &'a McpServerState, caller: &'a McpCaller) -> Self {
        Self { state, caller }
    }
}

pub(super) type ArgumentPreparer =
    for<'a> fn(&ToolArgumentContext<'a>, &Value) -> Result<Value, ToolError>;

impl<R: CommandRunner> McpServer<R> {
    pub(super) fn driver_authority_required_result(
        &self,
        run_id: &str,
        tool: &str,
    ) -> ToolCallResult {
        ToolCallResult::error(json!({
            "ok": false,
            "run_id": run_id,
            "error": {
                "code": "driver_authority_required",
                "message": format!("{tool} requires runtime driver authority")
            },
            "recovery": {
                "action": "run_flow",
                "reason": "production runtime tools must use a driver-owned run"
            }
        }))
    }

    pub(super) fn proxy_driver_owned_run_tool(
        &mut self,
        tool_name: &str,
        wire: DriverWire,
        prepare_arguments: ArgumentPreparer,
        arguments: &Value,
    ) -> Result<ToolCallResult, ToolError> {
        let context = ToolArgumentContext::new(&self.state, &self.caller);
        let driver_arguments = prepare_arguments(&context, arguments)?;
        let run_id = require_string(&driver_arguments, &["run_id"])?;
        let run_root = self
            .run_asset_store
            .run_root(run_id)
            .map_err(ToolError::from_run_asset)?;
        let attached = match self.attach_or_restart_driver(run_id) {
            Ok(attached) => attached,
            Err(err) => {
                persist_private_mcp_diagnostic(
                    &run_root,
                    "driver_recovery_failed",
                    json!({ "error": err.diagnostic() }),
                );
                return Ok(driver_unavailable_result(run_id));
            }
        };
        let Some(attached) = attached else {
            return Ok(self.driver_authority_required_result(run_id, tool_name));
        };
        let mut response = match attached
            .client
            .request(wire.as_str(), run_id, &driver_arguments)
        {
            Ok(response) => response,
            Err(err) => {
                persist_private_mcp_diagnostic(
                    &run_root,
                    "driver_request_failed",
                    json!({ "operation": wire.as_str(), "error": err.to_string() }),
                );
                return Ok(driver_unavailable_result(run_id));
            }
        };
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            let cleanup = self.cleanup_driver_response_panes(
                &run_root,
                &mut response,
                &[],
                "driver_proxy_error",
            );
            publish_cleanup_report(&mut response, cleanup);
        }
        sanitize_public_driver_response(&mut response);
        Ok(tool_result_from_driver_response(response))
    }
}

pub(super) fn prepare_authoring_arguments(
    _context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    Ok(Value::Object(argument_object(arguments)?))
}

pub(super) fn prepare_driver_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    Ok(Value::Object(driver_argument_object(context, arguments)?))
}

pub(super) fn prepare_artifact_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut object = driver_argument_object(context, arguments)?;
    insert_required_string(
        arguments,
        &mut object,
        "artifact_id",
        &["artifact_id", "artifact_key", "artifactKey", "key"],
    )?;
    Ok(Value::Object(object))
}

pub(super) fn prepare_fanout_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut object = driver_argument_object(context, arguments)?;
    insert_required_string(
        arguments,
        &mut object,
        "artifact_id",
        &["artifact_id", "artifact_key", "artifactKey", "key"],
    )?;
    insert_node_spec(arguments, &mut object)?;
    Ok(Value::Object(object))
}

pub(super) fn prepare_effect_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut object = driver_argument_object(context, arguments)?;
    insert_required_string(
        arguments,
        &mut object,
        "effect_key",
        &["effect_key", "effectKey", "key"],
    )?;
    Ok(Value::Object(object))
}

pub(super) fn prepare_hook_fact_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut object = driver_argument_object(context, arguments)?;
    canonicalize_hook_aliases(arguments, &mut object)?;
    Ok(Value::Object(object))
}

pub(super) fn prepare_board_patch_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut object = driver_argument_object(context, arguments)?;
    if let Some(expected_version) =
        optional_u64(arguments, &["expected_version", "expectedVersion"])?
    {
        object.insert("expected_version".into(), Value::from(expected_version));
    }
    Ok(Value::Object(object))
}

pub(super) fn prepare_node_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut object = driver_argument_object(context, arguments)?;
    insert_node_spec(arguments, &mut object)?;
    Ok(Value::Object(object))
}

pub(super) fn prepare_participant_message_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut object = driver_argument_object(context, arguments)?;
    insert_required_string(
        arguments,
        &mut object,
        "message_id",
        &["message_id", "messageId"],
    )?;
    let text = require_string(arguments, &["text", "message"])?;
    if text.trim().is_empty() {
        return Err(ToolError::invalid("text must be non-empty"));
    }
    object.insert("text".into(), Value::String(text.to_string()));
    Ok(Value::Object(object))
}

pub(super) fn prepare_apply_flow_lock_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut object = driver_argument_object(context, arguments)?;
    let mode = flow_lock_mode_arg(arguments)?;
    add_flow_lock_package_arguments(context, arguments, &mut object, mode)?;
    Ok(Value::Object(object))
}

pub(super) fn prepare_apply_flow_update_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut object = driver_argument_object(context, arguments)?;
    let lock_id = required_flow_lock_id(arguments)?;
    let mode = optional_flow_lock_mode_arg(arguments, &["apply_mode", "applyMode"])?
        .or_else(|| {
            context
                .state
                .proposed_updates
                .get(lock_id)
                .map(|proposal| proposal.mode)
        })
        .unwrap_or(crate::runtime::FlowLockMode::FutureActivations);
    add_flow_lock_package_arguments(context, arguments, &mut object, mode)?;
    Ok(Value::Object(object))
}

pub(super) fn prepare_preview_flow_routes_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut object = driver_argument_object(context, arguments)?;
    let Some(lock_id) = optional_string(
        arguments,
        &["flow_lock_id", "flowLockId", "lock_id", "lockId"],
    )?
    else {
        return Ok(Value::Object(object));
    };
    let provided_content_hash = require_string(arguments, &["content_hash", "contentHash"])?;
    let lock = context
        .state
        .flow_locks
        .get(lock_id)
        .ok_or_else(|| ToolError::invalid("flow lock not found"))?;
    let expected_content_hash = lock.content_hash();
    if provided_content_hash != expected_content_hash {
        return Err(ToolError::invalid("flow lock content hash mismatch"));
    }
    object.insert("flow_lock_id".into(), Value::String(lock_id.to_string()));
    object.insert(
        "content_hash".into(),
        Value::String(provided_content_hash.to_string()),
    );
    object.insert(
        "flow_lock".into(),
        super::driver_run_flow::flow_lock_package(lock, provided_content_hash)?,
    );
    Ok(Value::Object(object))
}

fn add_flow_lock_package_arguments(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
    object: &mut Map<String, Value>,
    mode: crate::runtime::FlowLockMode,
) -> Result<(), ToolError> {
    let lock_id = required_flow_lock_id(arguments)?;
    let provided_content_hash = optional_string(arguments, &["content_hash", "contentHash"])?
        .ok_or_else(|| ToolError::missing("content_hash"))?;
    let lock = context
        .state
        .flow_locks
        .get(lock_id)
        .ok_or_else(|| ToolError::invalid("flow lock not found"))?;
    let expected_content_hash = lock.content_hash();
    if provided_content_hash != expected_content_hash {
        return Err(ToolError::invalid("flow lock content hash mismatch"));
    }
    object.insert(
        "flow_lock".into(),
        super::driver_run_flow::flow_lock_package(lock, provided_content_hash)?,
    );
    object.insert(
        "review_id".into(),
        Value::String(require_string(arguments, &["review_id", "reviewId"])?.to_string()),
    );
    object.insert("mode".into(), json!(flow_lock_mode_name(mode)));
    Ok(())
}

fn required_flow_lock_id(arguments: &Value) -> Result<&str, ToolError> {
    optional_string(
        arguments,
        &["flow_lock_id", "flowLockId", "lock_id", "lockId"],
    )?
    .ok_or_else(|| ToolError::missing("flow_lock_id"))
}

fn argument_object(arguments: &Value) -> Result<Map<String, Value>, ToolError> {
    arguments
        .as_object()
        .cloned()
        .ok_or_else(|| ToolError::invalid("tool arguments must be an object"))
}

fn driver_argument_object(
    context: &ToolArgumentContext<'_>,
    arguments: &Value,
) -> Result<Map<String, Value>, ToolError> {
    let mut object = argument_object(arguments)?;
    match context.caller {
        McpCaller::Operator => {
            let run_id = require_string(arguments, &["run_id", "runId"])?;
            object.insert("run_id".into(), Value::String(run_id.to_string()));
        }
        McpCaller::Participant(participant) => {
            reject_forged_identity(
                arguments,
                &["run_id", "runId"],
                &participant.run_id,
                "run_id",
            )?;
            reject_forged_identity(
                arguments,
                &["activation_id", "activationId"],
                &participant.activation_id,
                "activation_id",
            )?;
            object.insert("run_id".into(), Value::String(participant.run_id.clone()));
            object.insert(
                "activation_id".into(),
                Value::String(participant.activation_id.clone()),
            );
            object.insert(
                "participant_handle".into(),
                Value::String(participant.handle.clone()),
            );
            object.insert(
                "participant_credential".into(),
                Value::String(participant.credential.clone()),
            );
        }
    }
    canonicalize_common_aliases(arguments, &mut object)?;
    Ok(object)
}

fn reject_forged_identity(
    arguments: &Value,
    aliases: &[&str],
    expected: &str,
    label: &str,
) -> Result<(), ToolError> {
    if let Some(actual) = optional_string(arguments, aliases)?
        && actual != expected
    {
        return Err(ToolError::invalid(format!(
            "{label} does not match participant binding"
        )));
    }
    Ok(())
}

fn canonicalize_common_aliases(
    arguments: &Value,
    object: &mut Map<String, Value>,
) -> Result<(), ToolError> {
    for (canonical, aliases) in [
        ("activation_id", &["activation_id", "activationId"][..]),
        ("node_id", &["node_id", "nodeId"][..]),
        ("session_id", &["session_id", "sessionId"][..]),
        (
            "source_native_id",
            &["source_native_id", "sourceNativeId"][..],
        ),
        ("causal_id", &["causal_id", "causalId"][..]),
        ("correlation_id", &["correlation_id", "correlationId"][..]),
    ] {
        if let Some(value) = optional_string(arguments, aliases)? {
            object.insert(canonical.into(), Value::String(value.to_string()));
        }
    }
    for (canonical, aliases) in [
        (
            "expected_event_cursor",
            &["expected_event_cursor", "expectedEventCursor"][..],
        ),
        (
            "expected_context_generation",
            &["expected_context_generation", "expectedContextGeneration"][..],
        ),
        (
            "activation_limit",
            &["activation_limit", "activationLimit"][..],
        ),
        (
            "stop_attempt_limit",
            &["stop_attempt_limit", "stopAttemptLimit"][..],
        ),
    ] {
        if let Some(value) = optional_u64(arguments, aliases)? {
            object.insert(canonical.into(), Value::from(value));
        }
    }
    Ok(())
}

fn canonicalize_hook_aliases(
    arguments: &Value,
    object: &mut Map<String, Value>,
) -> Result<(), ToolError> {
    insert_required_string(
        arguments,
        object,
        "session_id",
        &["session_id", "sessionId"],
    )?;
    let hook = require_string(arguments, &["hook"])?;
    if hook.trim().is_empty() {
        return Err(ToolError::invalid("hook must be non-empty"));
    }
    object.insert("hook".into(), Value::String(hook.to_string()));
    Ok(())
}

fn insert_node_spec(arguments: &Value, object: &mut Map<String, Value>) -> Result<(), ToolError> {
    let node_id = require_string(arguments, &["node_id", "nodeId"])?;
    if let Some(activation_id) = optional_string(arguments, &["activation_id", "activationId"])?
        && activation_id != node_id
    {
        return Err(ToolError::invalid(
            "activation_id must match node_id when using the public runtime API",
        ));
    }
    let node = node_spec_from_arguments(node_id, arguments)?;
    object.insert("node_id".into(), Value::String(node_id.to_string()));
    object.insert(
        "node_spec".into(),
        serde_json::to_value(node)
            .map_err(|err| ToolError::invalid(format!("node spec serialization failed: {err}")))?,
    );
    Ok(())
}

fn insert_required_string(
    arguments: &Value,
    object: &mut Map<String, Value>,
    canonical: &str,
    aliases: &[&str],
) -> Result<(), ToolError> {
    let value = require_string(arguments, aliases)?;
    if value.trim().is_empty() {
        return Err(ToolError::invalid(format!("{canonical} must be non-empty")));
    }
    object.insert(canonical.to_string(), Value::String(value.to_string()));
    Ok(())
}

fn tool_result_from_driver_response(response: Value) -> ToolCallResult {
    let ok = response.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if ok {
        ToolCallResult::ok(response)
    } else {
        ToolCallResult::error(response)
    }
}

fn driver_unavailable_result(run_id: &str) -> ToolCallResult {
    ToolCallResult::error(json!({
        "ok": false,
        "run_id": run_id,
        "error": {
            "code": "driver_unavailable",
            "message": "runtime driver automatic recovery is unavailable"
        },
        "recovery": {
            "action": "resume_run",
            "automatic_restart": true,
            "reason": "retry resume_run after the runtime driver becomes launchable"
        }
    }))
}
