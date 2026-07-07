use crate::flow;

use super::{RuntimeError, RuntimeState, activation_key, slot_index_key};

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize)]
pub struct RoutePreview {
    pub route_index: usize,
    pub activate: String,
    pub predicate: String,
    pub matched: bool,
    pub reason: Option<String>,
    pub for_each: Option<String>,
    pub planned_activations: Vec<PlannedActivationPreview>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize)]
pub struct PlannedActivationPreview {
    pub activation_id: String,
    pub stable_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
}

pub fn preview_flow_routes(
    state: &RuntimeState,
    run_id: &str,
    lock: &flow::FlowLock,
) -> Result<Vec<RoutePreview>, RuntimeError> {
    if !state.runs.contains(run_id) {
        return Err(RuntimeError::RunNotFound {
            run_id: run_id.to_owned(),
        });
    }

    Ok(lock
        .draft()
        .routes
        .iter()
        .enumerate()
        .map(|(route_index, route)| preview_route(state, run_id, route_index, route))
        .collect())
}

fn preview_route(
    state: &RuntimeState,
    run_id: &str,
    route_index: usize,
    route: &flow::FlowRoute,
) -> RoutePreview {
    let mut matched = false;
    let mut reason = None;
    let mut planned_activations = Vec::new();

    match evaluate_predicate(state, run_id, &route.predicate) {
        PredicateResult::Matched => match plan_activations(state, run_id, route) {
            Ok(plan) => {
                matched = true;
                planned_activations = plan;
            }
            Err(plan_reason) => {
                reason = Some(plan_reason);
            }
        },
        PredicateResult::Unmatched(predicate_reason) => {
            reason = Some(predicate_reason);
        }
    }

    RoutePreview {
        route_index,
        activate: route.activate.clone(),
        predicate: route.predicate.clone(),
        matched,
        reason,
        for_each: route.for_each.clone(),
        planned_activations,
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum PredicateResult {
    Matched,
    Unmatched(String),
}

fn evaluate_predicate(state: &RuntimeState, run_id: &str, predicate: &str) -> PredicateResult {
    let predicate = predicate.trim();

    if let Some(path) = exists_argument(predicate) {
        if let Some(key) = path.strip_prefix("artifact.") {
            return if latest_artifact_payload(state, run_id, key).is_some() {
                PredicateResult::Matched
            } else {
                PredicateResult::Unmatched("predicate_unmatched".into())
            };
        }
        if let Some(key) = path.strip_prefix("board.") {
            return if board_value(state, run_id, key).is_some() {
                PredicateResult::Matched
            } else {
                PredicateResult::Unmatched("predicate_unmatched".into())
            };
        }
        if bare_fact_path(path, "event.").is_some() {
            return PredicateResult::Unmatched("event fact source unavailable".into());
        }
    }

    if let Some(key) = bare_fact_path(predicate, "artifact.") {
        return if latest_artifact_payload(state, run_id, key).is_some_and(is_truthy_fact) {
            PredicateResult::Matched
        } else {
            PredicateResult::Unmatched("predicate_unmatched".into())
        };
    }

    if let Some(key) = bare_fact_path(predicate, "board.") {
        return if board_value(state, run_id, key).is_some_and(is_truthy_fact) {
            PredicateResult::Matched
        } else {
            PredicateResult::Unmatched("predicate_unmatched".into())
        };
    }

    if bare_fact_path(predicate, "event.").is_some() {
        return PredicateResult::Unmatched("event fact source unavailable".into());
    }

    PredicateResult::Unmatched("unsupported_predicate".into())
}

fn exists_argument(predicate: &str) -> Option<&str> {
    predicate
        .strip_prefix("exists(")?
        .strip_suffix(')')
        .map(str::trim)
        .filter(|path| !path.is_empty())
}

fn bare_fact_path<'a>(predicate: &'a str, prefix: &str) -> Option<&'a str> {
    let key = predicate.strip_prefix(prefix)?;
    if key.is_empty() || key.contains(|character: char| character.is_whitespace()) {
        return None;
    }
    if key
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '.')
    {
        Some(key)
    } else {
        None
    }
}

fn plan_activations(
    state: &RuntimeState,
    run_id: &str,
    route: &flow::FlowRoute,
) -> Result<Vec<PlannedActivationPreview>, String> {
    let Some(for_each) = route.for_each.as_deref() else {
        let activation_id = route.activate.clone();
        if state
            .activations
            .contains_key(&activation_key(run_id, &activation_id))
        {
            return Err(format!("duplicate activation: {activation_id}"));
        }
        return Ok(vec![PlannedActivationPreview {
            activation_id,
            stable_key: None,
            index: None,
            item: None,
        }]);
    };

    let artifact_key = for_each
        .trim()
        .strip_prefix("artifact.")
        .filter(|key| !key.is_empty())
        .ok_or_else(|| "unsupported_for_each".to_string())?;
    let payload = latest_artifact_payload(state, run_id, artifact_key)
        .ok_or_else(|| format!("artifact not found: {artifact_key}"))?;
    let mut planned = Vec::new();
    for (index, item) in payload.lines().enumerate() {
        let stable_key = format!("{artifact_key}/{index}");
        let activation_id = format!("{}:{stable_key}", route.activate);
        if state
            .activations
            .contains_key(&activation_key(run_id, &activation_id))
        {
            return Err(format!("duplicate activation: {activation_id}"));
        }
        planned.push(PlannedActivationPreview {
            activation_id,
            stable_key: Some(stable_key),
            index: Some(index),
            item: Some(item.to_owned()),
        });
    }
    Ok(planned)
}

fn latest_artifact_payload<'a>(
    state: &'a RuntimeState,
    run_id: &str,
    artifact_key: &str,
) -> Option<&'a str> {
    let artifact_id = state
        .latest_artifact_by_slot_index
        .get(&slot_index_key(run_id, artifact_key))?;
    state
        .artifact_records
        .get(artifact_id)
        .map(|artifact| artifact.payload.as_str())
}

fn board_value<'a>(state: &'a RuntimeState, run_id: &str, key: &str) -> Option<&'a str> {
    state
        .boards
        .get(run_id)?
        .get(key)
        .map(|value| value.as_str())
}

fn is_truthy_fact(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value != "false" && value != "0"
}
