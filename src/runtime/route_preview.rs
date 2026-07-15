use crate::flow;

use super::{RouteTrigger, RuntimeError, RuntimeState, next_activation_identity, slot_index_key};

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize)]
pub struct RoutePreview {
    pub route_index: usize,
    pub route_id: String,
    pub activate: String,
    pub predicate: String,
    pub matched: bool,
    pub reason: Option<String>,
    pub for_each: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<RouteTrigger>,
    pub planned_activations: Vec<PlannedActivationPreview>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize)]
pub struct PlannedActivationPreview {
    pub activation_id: String,
    pub stable_key: Option<String>,
    pub activation_generation: u64,
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
        .map(|(route_index, route)| preview_route(state, run_id, lock, route_index, route))
        .collect())
}

fn preview_route(
    state: &RuntimeState,
    run_id: &str,
    lock: &flow::FlowLock,
    route_index: usize,
    route: &flow::FlowRoute,
) -> RoutePreview {
    let route_id = flow::canonical_route_identity(route);
    let mut preview = RoutePreview {
        route_index,
        route_id: route_id.clone(),
        activate: route.activate.clone(),
        predicate: route.predicate.to_string(),
        matched: false,
        reason: None,
        for_each: route.for_each.as_ref().map(ToString::to_string),
        trigger: None,
        planned_activations: Vec::new(),
    };

    let predicate_fact = match evaluate_predicate(state, run_id, &route.predicate) {
        PredicateResult::Matched(fact) => fact,
        PredicateResult::Unmatched(reason) => {
            preview.reason = Some(reason);
            return preview;
        }
    };
    let trigger_fact = match route.for_each.as_ref() {
        Some(for_each) => match for_each_artifact_fact(state, run_id, for_each) {
            Ok(fact) => fact,
            Err(reason) => {
                preview.reason = Some(reason);
                return preview;
            }
        },
        None => predicate_fact,
    };
    let trigger = RouteTrigger {
        flow_lock_id: lock.id().to_owned(),
        route_id,
        fact_ref: trigger_fact.fact_ref,
        fact_version: trigger_fact.version,
    };
    preview.trigger = Some(trigger.clone());
    if state.has_applied_trigger(run_id, &trigger) {
        preview.reason = Some("trigger_already_applied".into());
        return preview;
    }

    match plan_activations(state, run_id, route) {
        Ok(planned) => {
            let remaining = state
                .activation_limit(run_id)
                .unwrap_or(u64::MAX)
                .saturating_sub(state.activations_used(run_id));
            preview.planned_activations = planned;
            if preview.planned_activations.len() as u64 > remaining {
                preview.reason = Some("activation_limit_exhausted".into());
            } else if preview.planned_activations.is_empty() {
                preview.reason = Some("no_activations".into());
            } else {
                preview.matched = true;
            }
        }
        Err(reason) => preview.reason = Some(reason),
    }
    preview
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct FactVersion {
    fact_ref: String,
    version: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum PredicateResult {
    Matched(FactVersion),
    Unmatched(String),
}

fn evaluate_predicate(
    state: &RuntimeState,
    run_id: &str,
    predicate: &flow::FlowPredicate,
) -> PredicateResult {
    match predicate.fact_ref() {
        flow::FactRef::Artifact { key } => {
            let artifact = latest_artifact(state, run_id, key.as_str());
            if !predicate.matches(artifact.map(|artifact| artifact.payload.as_str())) {
                return PredicateResult::Unmatched("predicate_unmatched".into());
            }
            let artifact = artifact.expect("a matching artifact predicate must have a value");
            PredicateResult::Matched(FactVersion {
                fact_ref: predicate.fact_ref().to_string(),
                version: artifact.event_sequence,
            })
        }
        flow::FactRef::Board { key } => {
            let value = board_value(state, run_id, key.as_str());
            if !predicate.matches(value) {
                return PredicateResult::Unmatched("predicate_unmatched".into());
            }
            let Some(version) = state.board_fact_version(run_id, key.as_str()) else {
                return PredicateResult::Unmatched("predicate_unmatched".into());
            };
            PredicateResult::Matched(FactVersion {
                fact_ref: predicate.fact_ref().to_string(),
                version,
            })
        }
    }
}

fn for_each_artifact_fact(
    state: &RuntimeState,
    run_id: &str,
    for_each: &flow::ArtifactRef,
) -> Result<FactVersion, String> {
    let key = for_each.key();
    let artifact = latest_artifact(state, run_id, key.as_str())
        .ok_or_else(|| format!("artifact not found: {key}"))?;
    Ok(FactVersion {
        fact_ref: for_each.to_string(),
        version: artifact.event_sequence,
    })
}

fn plan_activations(
    state: &RuntimeState,
    run_id: &str,
    route: &flow::FlowRoute,
) -> Result<Vec<PlannedActivationPreview>, String> {
    let Some(for_each) = route.for_each.as_ref() else {
        let (generation, activation_id) =
            next_activation_identity(state, run_id, &route.activate, None);
        return Ok(vec![PlannedActivationPreview {
            activation_id,
            stable_key: None,
            activation_generation: generation,
            index: None,
            item: None,
        }]);
    };

    let artifact_key = for_each.key().as_str();
    let payload = &latest_artifact(state, run_id, artifact_key)
        .ok_or_else(|| format!("artifact not found: {artifact_key}"))?
        .payload;
    Ok(payload
        .lines()
        .enumerate()
        .map(|(index, item)| {
            let stable_key = format!("{artifact_key}/{index}");
            let (generation, activation_id) =
                next_activation_identity(state, run_id, &route.activate, Some(&stable_key));
            PlannedActivationPreview {
                activation_id,
                stable_key: Some(stable_key),
                activation_generation: generation,
                index: Some(index),
                item: Some(item.to_owned()),
            }
        })
        .collect())
}

fn latest_artifact<'a>(
    state: &'a RuntimeState,
    run_id: &str,
    artifact_key: &str,
) -> Option<&'a super::ArtifactRecord> {
    let artifact_id = state
        .latest_artifact_by_slot_index
        .get(&slot_index_key(run_id, artifact_key))?;
    state.artifact_records.get(artifact_id)
}

fn board_value<'a>(state: &'a RuntimeState, run_id: &str, key: &str) -> Option<&'a str> {
    state.boards.get(run_id)?.get(key).map(String::as_str)
}
