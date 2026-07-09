use crate::runtime;
use crate::view::{
    PaneMappingSnapshot, RuntimeDecisionSnapshot, RuntimeEventSnapshot,
    RuntimeStopDecisionSnapshot, VisualizationSnapshot,
};

use super::McpServerState;

impl McpServerState {
    pub(super) fn enrich_runtime_snapshot(&self, snapshot: &mut VisualizationSnapshot) {
        for run in &mut snapshot.runs {
            if let Some(archive) = self.run_archives.get(&run.run_id) {
                run.flow_lock_id = Some(archive.flow_lock_id.clone());
                run.content_hash = Some(archive.content_hash.clone());
                run.flow_review_status = Some(archive.review_status.clone());
                run.flow_export_document = Some(archive.flow_export_document.clone());
            }
            if let Some(warnings) = self.actuation_warnings.get(&run.run_id) {
                run.actuation_warnings = warnings.clone();
            }

            let timeline = self
                .runtime()
                .events()
                .iter()
                .filter(|event| event.source.run_id.as_deref() == Some(run.run_id.as_str()))
                .map(runtime_event_snapshot)
                .collect::<Vec<_>>();
            run.event_count = timeline.len();
            run.last_decision = self.latest_stop_decision(&run.run_id);
            run.stop_decisions = self.stop_decision_snapshots(&run.run_id);
            run.event_timeline = timeline;
            if let Some(records) = self.machine_inputs.get(&run.run_id) {
                run.machine_inputs = records.clone();
            }

            let Some(window) = self.tmux_windows.get(&run.run_id) else {
                continue;
            };
            let mut pane_mappings = self
                .tmux_panes
                .iter()
                .filter(|((pane_run_id, _), _)| pane_run_id == &run.run_id)
                .map(|((_, activation_id), pane)| {
                    let status = run
                        .activations
                        .get(activation_id)
                        .map(|activation| activation.status.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    PaneMappingSnapshot {
                        activation_id: activation_id.clone(),
                        run_id: run.run_id.clone(),
                        pane: format!("{}:{}.{}", pane.session_id(), pane.window_id(), pane.id()),
                        session_id: pane.session_id().to_string(),
                        window_id: pane.window_id().to_string(),
                        window_name: window.name().to_string(),
                        pane_id: pane.id().to_string(),
                        status,
                    }
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
    }

    fn latest_stop_decision(&self, run_id: &str) -> Option<RuntimeDecisionSnapshot> {
        self.runtime()
            .events()
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                runtime::EventPayload::StopDecision {
                    run_id: event_run_id,
                    decision,
                    ..
                } if event_run_id == run_id => Some(RuntimeDecisionSnapshot {
                    decision_id: format!("event:{}", event.sequence),
                    summary: stop_decision_kind_text(decision.kind).to_string(),
                    why: decision
                        .reason
                        .clone()
                        .unwrap_or_else(|| "stop requirements satisfied".to_string()),
                }),
                _ => None,
            })
    }

    fn stop_decision_snapshots(&self, run_id: &str) -> Vec<RuntimeStopDecisionSnapshot> {
        self.runtime()
            .events()
            .iter()
            .filter_map(|event| match &event.payload {
                runtime::EventPayload::StopDecision {
                    run_id: event_run_id,
                    activation_id,
                    decision,
                } if event_run_id == run_id => Some(RuntimeStopDecisionSnapshot {
                    decision_id: format!("event:{}", event.sequence),
                    activation_id: activation_id.clone(),
                    decision: stop_decision_kind_text(decision.kind).to_string(),
                    attempt: decision.attempt,
                    reason: decision.reason.clone(),
                    missing: decision
                        .missing_artifacts
                        .iter()
                        .map(|artifact| format!("artifact:{artifact}"))
                        .chain(
                            decision
                                .missing_effects
                                .iter()
                                .map(|effect| format!("effect:{effect}")),
                        )
                        .collect(),
                }),
                _ => None,
            })
            .collect()
    }
}

fn runtime_event_snapshot(event: &runtime::Event) -> RuntimeEventSnapshot {
    match &event.payload {
        runtime::EventPayload::RunStarted { run_id } => RuntimeEventSnapshot {
            sequence: event.sequence,
            label: "run_started".to_string(),
            detail: format!("run {run_id} started"),
        },
        runtime::EventPayload::RunStatusChanged { run_id, status } => RuntimeEventSnapshot {
            sequence: event.sequence,
            label: "run_status_changed".to_string(),
            detail: format!("run {run_id} status {}", run_status_text(*status)),
        },
        runtime::EventPayload::NodeActivated {
            activation_id,
            node_id,
            ..
        } => RuntimeEventSnapshot {
            sequence: event.sequence,
            label: "node_activated".to_string(),
            detail: format!("activation {activation_id} node {node_id}"),
        },
        runtime::EventPayload::ActivationStatusChanged {
            activation_id,
            status,
            ..
        } => RuntimeEventSnapshot {
            sequence: event.sequence,
            label: "activation_status_changed".to_string(),
            detail: format!(
                "activation {activation_id} status {}",
                activation_status_text(*status)
            ),
        },
        runtime::EventPayload::ArtifactDelivered {
            activation_id,
            artifact_key,
            ..
        } => RuntimeEventSnapshot {
            sequence: event.sequence,
            label: "artifact_delivered".to_string(),
            detail: format!("activation {activation_id} artifact {artifact_key}"),
        },
        runtime::EventPayload::BoardPatched {
            activation_id, key, ..
        } => RuntimeEventSnapshot {
            sequence: event.sequence,
            label: "board_patched".to_string(),
            detail: format!("activation {activation_id} board {key}"),
        },
        runtime::EventPayload::StopObserved {
            activation_id,
            observation,
            ..
        } => RuntimeEventSnapshot {
            sequence: event.sequence,
            label: "stop_observed".to_string(),
            detail: format!("activation {activation_id} stop {}", observation.reason),
        },
        runtime::EventPayload::StopDecision {
            activation_id,
            decision,
            ..
        } => RuntimeEventSnapshot {
            sequence: event.sequence,
            label: "stop_decision".to_string(),
            detail: format!(
                "activation {activation_id} decision {}",
                stop_decision_kind_text(decision.kind)
            ),
        },
        runtime::EventPayload::EffectRecorded {
            activation_id,
            effect_key,
            ..
        } => RuntimeEventSnapshot {
            sequence: event.sequence,
            label: "effect_recorded".to_string(),
            detail: format!("activation {activation_id} effect {effect_key}"),
        },
        runtime::EventPayload::FlowApplied { lock_id, .. } => RuntimeEventSnapshot {
            sequence: event.sequence,
            label: "flow_applied".to_string(),
            detail: format!("flow {lock_id} applied"),
        },
        runtime::EventPayload::FlowUpdate {
            status, lock_id, ..
        } => RuntimeEventSnapshot {
            sequence: event.sequence,
            label: "flow_update".to_string(),
            detail: format!("flow {lock_id} {}", flow_update_status_text(*status)),
        },
    }
}

fn run_status_text(status: runtime::RunStatus) -> &'static str {
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

fn activation_status_text(status: runtime::ActivationStatus) -> &'static str {
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

fn flow_update_status_text(status: runtime::FlowUpdateStatus) -> &'static str {
    match status {
        runtime::FlowUpdateStatus::Proposed => "proposed",
        runtime::FlowUpdateStatus::Checked => "checked",
        runtime::FlowUpdateStatus::Applied => "applied",
    }
}

fn stop_decision_kind_text(kind: runtime::StopDecisionKind) -> &'static str {
    match kind {
        runtime::StopDecisionKind::Allow => "allow",
        runtime::StopDecisionKind::Deny => "deny",
        runtime::StopDecisionKind::Block => "block",
        runtime::StopDecisionKind::Yield => "yield",
    }
}
