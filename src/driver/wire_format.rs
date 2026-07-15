use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::runtime;

use super::DriverFailure;

pub(super) fn stop_validation_error_json(
    run_id: &str,
    activation_id: &str,
    err: runtime::StopValidationError,
) -> Value {
    let missing_detail = match &err {
        runtime::StopValidationError::MissingArtifact { artifact_key, .. } => {
            json!({ "artifact_key": artifact_key })
        }
        runtime::StopValidationError::MissingEffect { effect_key, .. } => {
            json!({ "effect_key": effect_key })
        }
        runtime::StopValidationError::ActivationNotFound { .. }
        | runtime::StopValidationError::ActivationNotFoundInRun { .. } => {
            json!({ "activation_id": activation_id })
        }
        runtime::StopValidationError::RunNotFound { .. } => json!({ "run_id": run_id }),
    };
    json!({
        "ok": false,
        "run_id": run_id,
        "activation_id": activation_id,
        "valid": false,
        "stop_valid": false,
        "missing": stop_validation_missing(&err),
        "missing_detail": missing_detail,
        "error": err.to_string()
    })
}

fn stop_validation_missing(err: &runtime::StopValidationError) -> Vec<String> {
    match err {
        runtime::StopValidationError::RunNotFound { .. }
        | runtime::StopValidationError::ActivationNotFound { .. }
        | runtime::StopValidationError::ActivationNotFoundInRun { .. } => {
            vec!["activation".to_string()]
        }
        runtime::StopValidationError::MissingArtifact { artifact_key, .. } => {
            vec![format!("artifact:{artifact_key}")]
        }
        runtime::StopValidationError::MissingEffect { effect_key, .. } => {
            vec![format!("effect:{effect_key}")]
        }
    }
}

pub(super) fn stop_decisions_json(decisions: &[runtime::StopDecision]) -> Vec<Value> {
    decisions
        .iter()
        .map(|decision| {
            json!({
                "kind": stop_decision_kind_name(decision.kind),
                "attempt": decision.attempt,
                "missing_artifacts": decision.missing_artifacts,
                "missing_effects": decision.missing_effects,
                "reason": decision.reason
            })
        })
        .collect()
}

pub(super) fn route_decisions_json(decisions: &[runtime::RouteDecision]) -> Vec<Value> {
    decisions
        .iter()
        .map(|decision| {
            json!({
                "run_id": decision.run_id,
                "flow_lock_id": decision.flow_lock_id,
                "route_index": decision.route_index,
                "route_id": decision.route_id,
                "trigger": decision.trigger,
                "predicate": decision.predicate,
                "for_each": decision.for_each,
                "activate": decision.applied_activation_ids,
                "planned_activation_ids": decision.planned_activation_ids,
                "applied_activation_ids": decision.applied_activation_ids
            })
        })
        .collect()
}

pub(super) fn effects_json(state: &runtime::RuntimeState, run_id: &str) -> BTreeMap<String, Value> {
    state
        .effects
        .iter()
        .filter(|((effect_run_id, _, _), _)| effect_run_id == run_id)
        .map(|((_, activation_id, effect_key), payload)| {
            (
                format!("{activation_id}/{effect_key}"),
                parse_payload_value(payload),
            )
        })
        .collect()
}

pub(super) fn parse_context_payloads(
    context: &BTreeMap<String, String>,
) -> BTreeMap<String, Value> {
    context
        .iter()
        .map(|(key, value)| (key.clone(), parse_payload_value(value)))
        .collect()
}

pub(super) fn parse_payload_value(payload: &str) -> Value {
    serde_json::from_str(payload).unwrap_or_else(|_| Value::String(payload.to_string()))
}

pub(super) fn payload_string(value: Option<&Value>) -> Result<String, DriverFailure> {
    match value {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(value) => serde_json::to_string(value)
            .map_err(|err| DriverFailure::new("malformed_request", err.to_string())),
        None => Ok("null".into()),
    }
}

pub(super) fn optional_u64_field(
    value: &Value,
    keys: &[&'static str],
) -> Result<Option<u64>, DriverFailure> {
    for key in keys {
        if let Some(field) = value.get(*key) {
            return field.as_u64().map(Some).ok_or_else(|| {
                DriverFailure::new(
                    "malformed_request",
                    format!("{key} must be an unsigned integer"),
                )
            });
        }
    }
    Ok(None)
}

pub(super) fn required_string<'a>(
    value: &'a Value,
    key: &'static str,
) -> Result<&'a str, DriverFailure> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| DriverFailure::new("malformed_request", format!("{key} is required")))
}

pub(super) fn string_field<'a>(
    object: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a str, DriverFailure> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| DriverFailure::new("malformed_request", format!("{key} is required")))
}

pub(super) fn with_id(id: Value, response: Value) -> Value {
    let mut object = match response {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    object.insert("id".into(), id);
    Value::Object(object)
}

pub(super) fn driver_error(id: Value, code: &str, message: &str) -> Value {
    json!({
        "id": id,
        "ok": false,
        "error": {
            "code": code,
            "message": message
        }
    })
}

pub(super) fn run_status_name(status: runtime::RunStatus) -> &'static str {
    match status {
        runtime::RunStatus::PendingReview => "pending_review",
        runtime::RunStatus::Ready => "ready",
        runtime::RunStatus::Running => "running",
        runtime::RunStatus::Paused => "paused",
        runtime::RunStatus::Blocked => "blocked",
        runtime::RunStatus::Quiescent => "quiescent",
        runtime::RunStatus::Completed => "completed",
        runtime::RunStatus::Failed => "failed",
        runtime::RunStatus::Stopping => "stopping",
        runtime::RunStatus::Stopped => "stopped",
    }
}

pub(super) fn run_mode_name(mode: runtime::RunMode) -> &'static str {
    match mode {
        runtime::RunMode::Finite => "finite",
        runtime::RunMode::Continuous => "continuous",
        runtime::RunMode::Manual => "manual",
    }
}

pub(super) fn activation_status_name(status: runtime::ActivationStatus) -> &'static str {
    match status {
        runtime::ActivationStatus::Pending => "pending",
        runtime::ActivationStatus::Starting => "starting",
        runtime::ActivationStatus::Running => "running",
        runtime::ActivationStatus::WaitingForStop => "waiting_for_stop",
        runtime::ActivationStatus::ValidatingStop => "validating_stop",
        runtime::ActivationStatus::Blocked => "blocked",
        runtime::ActivationStatus::Completed => "completed",
        runtime::ActivationStatus::Failed => "failed",
        runtime::ActivationStatus::Cancelled => "cancelled",
    }
}

pub(super) fn flow_lock_mode_name(mode: runtime::FlowLockMode) -> &'static str {
    match mode {
        runtime::FlowLockMode::FutureActivations => "future_activations",
        runtime::FlowLockMode::CheckpointRestart => "checkpoint_restart",
    }
}

pub(super) fn stop_decision_kind_name(kind: runtime::StopDecisionKind) -> &'static str {
    match kind {
        runtime::StopDecisionKind::Allow => "allow",
        runtime::StopDecisionKind::Deny => "deny",
        runtime::StopDecisionKind::Block => "block",
        runtime::StopDecisionKind::Yield => "yield",
    }
}
