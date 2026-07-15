use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::runtime;
use crate::view::{PaneMappingSnapshot, VisualizationSnapshot, render_terminal_dashboard};

use super::flow_lock::StoredFlowRevision;
use super::{
    AmbiguousDelivery, DriverFailure, RuntimeDriverService, activation_status_name, effects_json,
    flow_lock_mode_name, parse_context_payloads, parse_payload_value, required_string,
    run_mode_name, run_status_name, stop_validation_error_json,
};

struct AuthoritativeReadSnapshot {
    run_id: String,
    runtime: runtime::Runtime,
    locks: BTreeMap<String, StoredFlowRevision>,
    visualization: VisualizationSnapshot,
    context: Value,
    run_status: runtime::RunStatus,
    event_cursor: u64,
    context_generation: u64,
    has_run: bool,
}

impl AuthoritativeReadSnapshot {
    fn require_run(&self) -> Result<(), DriverFailure> {
        if self.has_run {
            Ok(())
        } else {
            Err(DriverFailure::new("run_not_found", "run not found"))
        }
    }

    fn with_authority_fields(&self, mut response: Value) -> Value {
        if let Value::Object(object) = &mut response {
            object.insert("event_cursor".into(), Value::from(self.event_cursor));
            object.insert(
                "context_generation".into(),
                Value::from(self.context_generation),
            );
        }
        response
    }
}

impl RuntimeDriverService {
    pub(super) fn get_context_response(&self, request: &Value) -> Result<Value, DriverFailure> {
        let snapshot = self.authoritative_read_snapshot()?;
        snapshot.require_run()?;
        let participant_activation_id = request
            .get("participant_handle")
            .and_then(Value::as_str)
            .map(|_| required_string(request, "activation_id"))
            .transpose()?;
        if let Some(activation_id) = participant_activation_id {
            let context = participant_context_projection(snapshot.context.clone(), activation_id);
            return Ok(snapshot.with_authority_fields(json!({
                "ok": true,
                "context": context
            })));
        }
        Ok(snapshot.with_authority_fields(json!({
            "ok": true,
            "run_id": snapshot.run_id,
            "context": snapshot.context
        })))
    }

    pub(super) fn status_response(&self) -> Result<Value, DriverFailure> {
        let snapshot = self.authoritative_read_snapshot()?;
        let state = snapshot.runtime.state();
        Ok(snapshot.with_authority_fields(json!({
            "ok": true,
            "run_id": snapshot.run_id,
            "run_status": run_status_name(snapshot.run_status),
            "run_status_reason": state.run_status_reason(&snapshot.run_id),
            "run_mode": state.run_mode(&snapshot.run_id).map(run_mode_name),
            "initial_activation_limit": state.initial_activation_limit(&snapshot.run_id),
            "activation_limit": state.activation_limit(&snapshot.run_id),
            "stop_attempt_limit": state.stop_attempt_limit(&snapshot.run_id),
            "activations_used": state.activations_used(&snapshot.run_id),
            "context": snapshot.context
        })))
    }

    pub(super) fn why_response(&self) -> Result<Value, DriverFailure> {
        let snapshot = self.authoritative_read_snapshot()?;
        snapshot.require_run()?;
        Ok(snapshot.with_authority_fields(json!({
            "ok": true,
            "run_id": snapshot.run_id,
            "run_status": run_status_name(snapshot.run_status),
            "cause": snapshot
                .runtime
                .state()
                .run_status_reason(&snapshot.run_id)
                .map(str::to_string)
                .unwrap_or_else(|| format!("run is {}", run_status_name(snapshot.run_status)))
        })))
    }

    pub(super) fn validate_stop(&self, request: &Value) -> Result<Value, DriverFailure> {
        let snapshot = self.authoritative_read_snapshot()?;
        snapshot.require_run()?;
        let activation_id = required_string(request, "activation_id")?;
        let response = match snapshot
            .runtime
            .validate_stop(&snapshot.run_id, activation_id)
        {
            Ok(()) => json!({
                "ok": true,
                "run_id": snapshot.run_id,
                "activation_id": activation_id,
                "valid": true,
                "stop_valid": true,
                "missing": []
            }),
            Err(err) => stop_validation_error_json(&snapshot.run_id, activation_id, err),
        };
        Ok(snapshot.with_authority_fields(response))
    }

    pub(super) fn preview_flow_routes(&self, request: &Value) -> Result<Value, DriverFailure> {
        let snapshot = self.authoritative_read_snapshot()?;
        snapshot.require_run()?;
        let (package, source) = if request.get("flow_lock").is_some() {
            (
                StoredFlowRevision::from_preview_request(request)?,
                "explicit",
            )
        } else {
            let state = snapshot.runtime.state();
            let application_id = state
                .latest_flow_lock_application_by_run
                .get(&snapshot.run_id)
                .ok_or_else(|| {
                    DriverFailure::new("flow_lock_required", "flow_lock_id is required")
                })?;
            let application = state
                .flow_lock_applications
                .get(application_id)
                .ok_or_else(|| {
                    DriverFailure::new("flow_lock_not_found", "latest applied flow lock not found")
                })?;
            let package = snapshot
                .locks
                .get(&application.lock_id)
                .cloned()
                .ok_or_else(|| {
                    DriverFailure::new("flow_lock_not_found", "flow lock package not found")
                })?;
            if package.content_hash() != application.content_hash {
                return Err(DriverFailure::new(
                    "flow_lock_hash_mismatch",
                    "latest applied flow lock content hash mismatch",
                ));
            }
            (package, "latest_applied")
        };
        if let Some(lock_id) = request.get("flow_lock_id").and_then(Value::as_str)
            && lock_id != package.lock_id()
        {
            return Err(DriverFailure::new(
                "flow_lock_identity_mismatch",
                "flow lock id does not match exact package",
            ));
        }
        if let Some(content_hash) = request.get("content_hash").and_then(Value::as_str)
            && content_hash != package.content_hash()
        {
            return Err(DriverFailure::new(
                "flow_lock_hash_mismatch",
                "flow lock content hash mismatch",
            ));
        }
        let lock = package.lock()?;
        let routes =
            runtime::preview_flow_routes(snapshot.runtime.state(), &snapshot.run_id, &lock)
                .map_err(DriverFailure::from_runtime)?;
        Ok(snapshot.with_authority_fields(json!({
            "ok": true,
            "run_id": snapshot.run_id,
            "flow_lock_id": package.lock_id(),
            "lock_id": package.lock_id(),
            "content_hash": package.content_hash(),
            "source": source,
            "routes": routes
        })))
    }

    pub(super) fn view_terminal_response(&self) -> Result<Value, DriverFailure> {
        let snapshot = self.authoritative_read_snapshot()?;
        snapshot.require_run()?;
        let dashboard = render_terminal_dashboard(&snapshot.visualization);
        Ok(snapshot.with_authority_fields(json!({
            "ok": true,
            "format": "terminal",
            "dashboard": dashboard,
            "run_count": snapshot.visualization.runs.len()
        })))
    }

    pub(super) fn view_snapshot_response(&self) -> Result<Value, DriverFailure> {
        let snapshot = self.authoritative_read_snapshot()?;
        snapshot.require_run()?;
        Ok(snapshot.with_authority_fields(json!({
            "ok": true,
            "format": "json",
            "run_count": snapshot.visualization.runs.len(),
            "snapshot": snapshot.visualization
        })))
    }

    pub(super) fn authoritative_context_for_cache(&self) -> Result<Value, DriverFailure> {
        let snapshot = self.authoritative_read_snapshot()?;
        snapshot.require_run()?;
        Ok(snapshot.context)
    }

    fn authoritative_read_snapshot(&self) -> Result<AuthoritativeReadSnapshot, DriverFailure> {
        let runtime = self.driver.runtime().clone();
        let run_id = self.config.run_id.clone();
        let has_run = runtime.has_run(&run_id);
        let run_status = runtime
            .state()
            .run_status(&run_id)
            .unwrap_or(runtime::RunStatus::PendingReview);
        let event_cursor = runtime
            .events()
            .last()
            .map(|event| event.sequence)
            .unwrap_or(0);
        let context_generation = event_cursor.saturating_add(self.driver_event_count);
        let locks = self.locks.clone();
        let context = self.read_context_json(
            &runtime,
            &locks,
            run_status,
            event_cursor,
            context_generation,
        )?;
        let visualization = self.read_visualization(&runtime, &locks)?;
        Ok(AuthoritativeReadSnapshot {
            run_id,
            runtime,
            locks,
            visualization,
            context,
            run_status,
            event_cursor,
            context_generation,
            has_run,
        })
    }

    fn read_context_json(
        &self,
        runtime: &runtime::Runtime,
        locks: &BTreeMap<String, StoredFlowRevision>,
        run_status: runtime::RunStatus,
        event_cursor: u64,
        context_generation: u64,
    ) -> Result<Value, DriverFailure> {
        let state = runtime.state();
        let run_id = self.config.run_id.as_str();
        let activations = state
            .activations
            .values()
            .filter(|activation| activation.run_id == run_id)
            .map(|activation| {
                (
                    activation.activation_id.clone(),
                    json!({
                        "activation_id": activation.activation_id,
                        "node_id": activation.node_id,
                        "stable_key": activation.stable_key,
                        "activation_generation": activation.activation_generation,
                        "trigger": activation.trigger,
                        "status": activation_status_name(activation.status),
                        "context": parse_context_payloads(&activation.context),
                        "participant": self
                            .participant_status_projection(&activation.activation_id),
                        "stop_attempts": state
                            .stop_validation_attempts
                            .get(&(run_id.to_string(), activation.activation_id.clone()))
                            .copied()
                            .unwrap_or(0)
                    }),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let artifacts = state
            .artifact_records
            .values()
            .filter(|artifact| artifact.run_id == run_id)
            .map(|artifact| {
                (
                    artifact.artifact_key.clone(),
                    parse_payload_value(&artifact.payload),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut flow_applications = state
            .flow_lock_applications
            .values()
            .filter(|application| application.run_id == run_id)
            .collect::<Vec<_>>();
        flow_applications.sort_by_key(|application| application.event_sequence);
        let flow_revisions = flow_applications
            .into_iter()
            .map(|application| {
                let review = locks
                    .get(&application.lock_id)
                    .map(|package| Value::String(package.review_id().to_string()))
                    .unwrap_or(Value::Null);
                json!({
                    "revision_id": application.application_id,
                    "flow_lock_id": application.lock_id,
                    "content_hash": application.content_hash,
                    "mode": flow_lock_mode_name(application.mode),
                    "event_sequence": application.event_sequence,
                    "review": review
                })
            })
            .collect::<Vec<_>>();
        let manifest = self
            .run_asset_store
            .load_manifest(run_id)
            .map_err(DriverFailure::from_run_asset)?;
        let mut run_assets = self
            .run_asset_store
            .manifest_json(&manifest)
            .map_err(DriverFailure::from_run_asset)?;
        if let Some(activations) = run_assets
            .get_mut("activations")
            .and_then(Value::as_object_mut)
        {
            for activation in activations.values_mut() {
                if let Some(activation) = activation.as_object_mut() {
                    activation.remove("readiness_nonce");
                }
            }
        }
        Ok(json!({
            "run_id": run_id,
            "run_status": run_status_name(run_status),
            "run_status_reason": state.run_status_reason(run_id),
            "run_mode": state.run_mode(run_id).map(run_mode_name),
            "initial_activation_limit": state.initial_activation_limit(run_id),
            "activation_limit": state.activation_limit(run_id),
            "stop_attempt_limit": state.stop_attempt_limit(run_id),
            "activations_used": state.activations_used(run_id),
            "event_cursor": event_cursor,
            "context_generation": context_generation,
            "activations": activations,
            "artifacts": artifacts,
            "artifact_versions": state
                .latest_artifact_by_slot_index
                .iter()
                .filter_map(|((fact_run_id, key), artifact_id)| {
                    (fact_run_id == run_id).then(|| {
                        state
                            .artifact_records
                            .get(artifact_id)
                            .map(|artifact| (key.clone(), artifact.event_sequence))
                    })?
                })
                .collect::<BTreeMap<_, _>>(),
            "board": state.boards.get(run_id).cloned().unwrap_or_default(),
            "board_versions": state
                .board_fact_versions
                .iter()
                .filter_map(|((fact_run_id, key), version)| {
                    (fact_run_id == run_id).then_some((key.clone(), *version))
                })
                .collect::<BTreeMap<_, _>>(),
            "effects": effects_json(state, run_id),
            "flow_revisions": flow_revisions,
            "ambiguous_deliveries": self
                .ambiguous_deliveries
                .values()
                .map(AmbiguousDelivery::to_json)
                .collect::<Vec<_>>(),
            "run_assets": run_assets
        }))
    }

    fn read_visualization(
        &self,
        runtime: &runtime::Runtime,
        locks: &BTreeMap<String, StoredFlowRevision>,
    ) -> Result<VisualizationSnapshot, DriverFailure> {
        let mut message_counts = BTreeMap::new();
        message_counts.insert(self.config.run_id.clone(), self.participant_message_count());
        let mut snapshot = VisualizationSnapshot::from_runtime(runtime.state(), &message_counts);
        snapshot.runs.retain(|run| run.run_id == self.config.run_id);
        let Some(run) = snapshot.run_mut(&self.config.run_id) else {
            return Ok(snapshot);
        };
        run.driver_mode = "authoritative_driver".to_string();
        run.driver_mode_detail =
            "progress and effects are owned by the per-run runtime driver".to_string();
        run.event_count = runtime
            .events()
            .iter()
            .filter(|event| event.source.run_id.as_deref() == Some(self.config.run_id.as_str()))
            .count();
        if let Some(lock_id) = run.flow_lock_id.as_deref()
            && let Some(package) = locks.get(lock_id)
        {
            run.flow_review_status = self
                .review_store
                .load(package.review_id())
                .ok()
                .map(|review| review.status().as_str().to_string());
        }
        if let Some(tmux) = &self.tmux {
            let mut pane_mappings = tmux
                .panes
                .iter()
                .map(|(activation_id, pane)| PaneMappingSnapshot {
                    activation_id: activation_id.clone(),
                    run_id: self.config.run_id.clone(),
                    pane: format!("{}:{}.{}", pane.session_id, pane.window_id, pane.pane_id),
                    session_id: pane.session_id.clone(),
                    window_id: pane.window_id.clone(),
                    window_name: pane.window_name.clone(),
                    pane_id: pane.pane_id.clone(),
                    status: run
                        .activations
                        .get(activation_id)
                        .map(|activation| activation.status.clone())
                        .unwrap_or_else(|| "unknown".to_string()),
                })
                .collect::<Vec<_>>();
            pane_mappings.sort_by(|left, right| left.activation_id.cmp(&right.activation_id));
            for mapping in &pane_mappings {
                if let Some(activation) = run.activations.get_mut(&mapping.activation_id) {
                    activation.pane = Some(mapping.clone());
                }
            }
            run.pane_mappings = pane_mappings;
        }
        run.actuation_warnings = self
            .ambiguous_deliveries
            .values()
            .map(|delivery| delivery.to_json())
            .collect();
        Ok(snapshot)
    }
}

fn participant_context_projection(mut context: Value, activation_id: &str) -> Value {
    let Some(context) = context.as_object_mut() else {
        return context;
    };
    for key in [
        "run_id",
        "run_assets",
        "flow_revisions",
        "ambiguous_deliveries",
    ] {
        context.remove(key);
    }
    if let Some(activations) = context
        .get_mut("activations")
        .and_then(Value::as_object_mut)
    {
        activations.retain(|key, _| key == activation_id);
    }
    Value::Object(context.clone())
}
