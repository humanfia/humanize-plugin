use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::flow::{ArtifactRef, FactError, FactKey};

mod driver;
mod identity;
mod route_preview;
mod stop;

use identity::{
    activation_id_for, activation_key, artifact_id, content_hash, effect_index_key,
    flow_lock_application_id, next_activation_identity, slot_index_key, stop_fact_id,
};

pub use driver::{
    ControlCommand, DriverRender, DriverState, DriverTickInput, DriverTickReport, LoopBudget,
    RouteDecision, RouteSourceArtifact, RunCompletionMode, TickBudget,
};
pub use route_preview::{PlannedActivationPreview, RoutePreview, preview_flow_routes};
pub use stop::{StopDecision, StopDecisionKind, StopObservation};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub sequence: u64,
    pub source: EventSource,
    pub kind: EventKind,
    pub strength: EventStrength,
    pub actor: Option<String>,
    pub correlation: Option<String>,
    pub payload: EventPayload,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventSource {
    pub run_id: Option<String>,
    pub activation_id: Option<String>,
    pub source_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    ActivationStatusChanged,
    ArtifactDelivered,
    BoardPatched,
    EffectRecorded,
    FlowApplied,
    FlowUpdate,
    NodeActivated,
    ParticipantExited,
    RunActivationLimitChanged,
    RunStarted,
    RunStatusChanged,
    StopDecision,
    StopObserved,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStrength {
    Applied,
    Checked,
    Decision,
    Observed,
    Proposed,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    RunStarted {
        run_id: String,
        #[serde(default)]
        mode: RunMode,
        #[serde(default = "unbounded_activation_limit")]
        activation_limit: u64,
        #[serde(default = "default_stop_attempt_limit")]
        stop_attempt_limit: u32,
    },
    RunActivationLimitChanged {
        run_id: String,
        activation_limit: u64,
    },
    RunStatusChanged {
        run_id: String,
        status: RunStatus,
        #[serde(default)]
        reason: Option<String>,
    },
    NodeActivated {
        run_id: String,
        activation_id: String,
        node_id: String,
        stable_key: Option<String>,
        #[serde(default)]
        activation_generation: u64,
        #[serde(default)]
        trigger: Option<RouteTrigger>,
        context: BTreeMap<String, String>,
        stop_contract: StopContract,
        flow_lock_mode: Option<FlowLockMode>,
        flow_lock_id: Option<String>,
        contract_hash: Option<String>,
    },
    ActivationStatusChanged {
        run_id: String,
        activation_id: String,
        status: ActivationStatus,
    },
    ParticipantExited {
        run_id: String,
        activation_id: String,
        allocation_generation: u64,
        exit_status: i32,
    },
    ArtifactDelivered {
        run_id: String,
        activation_id: String,
        artifact_id: String,
        artifact_key: String,
        content_hash: String,
        payload: String,
    },
    BoardPatched {
        run_id: String,
        activation_id: String,
        key: String,
        value: String,
        version: u64,
    },
    StopObserved {
        run_id: String,
        activation_id: String,
        observation: StopObservation,
    },
    StopDecision {
        run_id: String,
        activation_id: String,
        decision: StopDecision,
    },
    EffectRecorded {
        run_id: String,
        activation_id: String,
        effect_key: String,
        payload: String,
    },
    FlowApplied {
        run_id: String,
        mode: FlowLockMode,
        lock_id: String,
        content_hash: String,
    },
    FlowUpdate {
        run_id: String,
        status: FlowUpdateStatus,
        mode: FlowLockMode,
        lock_id: String,
        contract_hash: String,
    },
}

impl EventPayload {
    fn source(&self) -> EventSource {
        match self {
            EventPayload::RunStarted { run_id, .. }
            | EventPayload::RunActivationLimitChanged { run_id, .. }
            | EventPayload::RunStatusChanged { run_id, .. }
            | EventPayload::FlowApplied { run_id, .. }
            | EventPayload::FlowUpdate { run_id, .. } => EventSource {
                run_id: Some(run_id.clone()),
                activation_id: None,
                source_id: None,
            },
            EventPayload::NodeActivated {
                run_id,
                activation_id,
                ..
            }
            | EventPayload::ActivationStatusChanged {
                run_id,
                activation_id,
                ..
            }
            | EventPayload::ParticipantExited {
                run_id,
                activation_id,
                ..
            }
            | EventPayload::ArtifactDelivered {
                run_id,
                activation_id,
                ..
            }
            | EventPayload::BoardPatched {
                run_id,
                activation_id,
                ..
            }
            | EventPayload::StopObserved {
                run_id,
                activation_id,
                ..
            }
            | EventPayload::StopDecision {
                run_id,
                activation_id,
                ..
            }
            | EventPayload::EffectRecorded {
                run_id,
                activation_id,
                ..
            } => EventSource {
                run_id: Some(run_id.clone()),
                activation_id: Some(activation_id.clone()),
                source_id: None,
            },
        }
    }

    fn kind(&self) -> EventKind {
        match self {
            EventPayload::RunStarted { .. } => EventKind::RunStarted,
            EventPayload::RunActivationLimitChanged { .. } => EventKind::RunActivationLimitChanged,
            EventPayload::RunStatusChanged { .. } => EventKind::RunStatusChanged,
            EventPayload::NodeActivated { .. } => EventKind::NodeActivated,
            EventPayload::ActivationStatusChanged { .. } => EventKind::ActivationStatusChanged,
            EventPayload::ParticipantExited { .. } => EventKind::ParticipantExited,
            EventPayload::ArtifactDelivered { .. } => EventKind::ArtifactDelivered,
            EventPayload::BoardPatched { .. } => EventKind::BoardPatched,
            EventPayload::StopObserved { .. } => EventKind::StopObserved,
            EventPayload::StopDecision { .. } => EventKind::StopDecision,
            EventPayload::EffectRecorded { .. } => EventKind::EffectRecorded,
            EventPayload::FlowApplied { .. } => EventKind::FlowApplied,
            EventPayload::FlowUpdate { .. } => EventKind::FlowUpdate,
        }
    }

    fn strength(&self) -> EventStrength {
        match self {
            EventPayload::StopObserved { .. } => EventStrength::Observed,
            EventPayload::StopDecision { .. } => EventStrength::Decision,
            EventPayload::FlowUpdate { status, .. } => match status {
                FlowUpdateStatus::Proposed => EventStrength::Proposed,
                FlowUpdateStatus::Checked => EventStrength::Checked,
                FlowUpdateStatus::Applied => EventStrength::Applied,
            },
            _ => EventStrength::Applied,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    #[default]
    Finite,
    Continuous,
    Manual,
}

const fn unbounded_activation_limit() -> u64 {
    u64::MAX
}

const fn default_stop_attempt_limit() -> u32 {
    3
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct RouteTrigger {
    pub flow_lock_id: String,
    pub route_id: String,
    pub fact_ref: String,
    pub fact_version: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowLockMode {
    FutureActivations,
    CheckpointRestart,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowUpdateStatus {
    Proposed,
    Checked,
    Applied,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalEventStore {
    events: Vec<Event>,
}

impl LocalEventStore {
    pub fn append(&mut self, payload: EventPayload) -> Event {
        let event = Event {
            sequence: self.events.len() as u64 + 1,
            source: payload.source(),
            kind: payload.kind(),
            strength: payload.strength(),
            actor: Some("runtime".into()),
            correlation: None,
            payload,
        };
        self.events.push(event.clone());
        event
    }

    pub fn replay(&self) -> &[Event] {
        &self.events
    }
}

pub trait EventStore {
    fn events(&self) -> &[Event];
}

impl EventStore for LocalEventStore {
    fn events(&self) -> &[Event] {
        self.replay()
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeSpec {
    id: String,
    stop_contract: StopContract,
    for_each: Option<ArtifactRef>,
}

impl NodeSpec {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            stop_contract: StopContract::default(),
            for_each: None,
        }
    }

    pub fn with_stop_contract(mut self, stop_contract: StopContract) -> Self {
        self.stop_contract = stop_contract;
        self
    }

    pub fn with_for_each(mut self, artifact: ArtifactRef) -> Self {
        self.for_each = Some(artifact);
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn stop_contract(&self) -> &StopContract {
        &self.stop_contract
    }

    pub fn for_each_key(&self) -> Option<&str> {
        self.for_each
            .as_ref()
            .map(ArtifactRef::key)
            .map(FactKey::as_str)
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StopContract {
    required_artifacts: Vec<String>,
    required_effects: Vec<String>,
}

impl StopContract {
    pub fn new<Artifacts, Effects, Artifact, Effect>(
        required_artifacts: Artifacts,
        required_effects: Effects,
    ) -> Self
    where
        Artifacts: IntoIterator<Item = Artifact>,
        Artifact: Into<String>,
        Effects: IntoIterator<Item = Effect>,
        Effect: Into<String>,
    {
        Self {
            required_artifacts: required_artifacts.into_iter().map(Into::into).collect(),
            required_effects: required_effects.into_iter().map(Into::into).collect(),
        }
    }

    pub fn required_artifacts(&self) -> &[String] {
        &self.required_artifacts
    }

    pub fn required_effects(&self) -> &[String] {
        &self.required_effects
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    #[default]
    PendingReview,
    Ready,
    Running,
    Paused,
    Blocked,
    Quiescent,
    Completed,
    Failed,
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum SchedulingIntent {
    Explicit,
    FactTriggeredRoute,
}

pub(crate) fn scheduling_enabled(
    state: &RuntimeState,
    run_id: &str,
    intent: SchedulingIntent,
) -> bool {
    match state.run_status(run_id) {
        Some(RunStatus::Running) => true,
        Some(RunStatus::Quiescent) if intent == SchedulingIntent::FactTriggeredRoute => {
            state.run_mode(run_id) == Some(RunMode::Continuous)
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationStatus {
    #[default]
    Pending,
    Starting,
    Running,
    WaitingForStop,
    ValidatingStop,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Activation {
    pub activation_id: String,
    pub run_id: String,
    pub node_id: String,
    pub stable_key: Option<String>,
    pub activation_generation: u64,
    pub trigger: Option<RouteTrigger>,
    pub status: ActivationStatus,
    pub context: BTreeMap<String, String>,
    pub stop_contract: StopContract,
    pub flow_lock_mode: Option<FlowLockMode>,
    pub flow_lock_id: Option<String>,
    pub contract_hash: Option<String>,
}

impl Default for Activation {
    fn default() -> Self {
        Self {
            activation_id: String::new(),
            run_id: String::new(),
            node_id: String::new(),
            stable_key: None,
            activation_generation: 0,
            trigger: None,
            status: ActivationStatus::Pending,
            context: BTreeMap::new(),
            stop_contract: StopContract::default(),
            flow_lock_mode: None,
            flow_lock_id: None,
            contract_hash: None,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ArtifactRecord {
    pub artifact_id: String,
    pub run_id: String,
    pub activation_id: String,
    pub artifact_key: String,
    pub content_hash: String,
    pub payload: String,
    pub event_sequence: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowLockApplication {
    pub application_id: String,
    pub run_id: String,
    pub mode: FlowLockMode,
    pub lock_id: String,
    pub content_hash: String,
    pub event_sequence: u64,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct RuntimeState {
    pub run_id: Option<String>,
    pub runs: BTreeSet<String>,
    pub artifact_records: BTreeMap<String, ArtifactRecord>,
    pub latest_artifact_by_slot_index: BTreeMap<(String, String), String>,
    pub board: BTreeMap<String, String>,
    pub boards: BTreeMap<String, BTreeMap<String, String>>,
    pub board_version: u64,
    pub board_versions: BTreeMap<String, u64>,
    pub board_fact_versions: BTreeMap<(String, String), u64>,
    pub run_statuses: BTreeMap<String, RunStatus>,
    pub run_status_reasons: BTreeMap<String, String>,
    pub run_modes: BTreeMap<String, RunMode>,
    pub initial_activation_limits: BTreeMap<String, u64>,
    pub activation_limits: BTreeMap<String, u64>,
    pub stop_attempt_limits: BTreeMap<String, u32>,
    pub activations: BTreeMap<(String, String), Activation>,
    pub effects: BTreeMap<(String, String, String), String>,
    pub stop_observations: BTreeMap<String, StopObservation>,
    pub stop_decisions: BTreeMap<String, StopDecision>,
    pub stop_validation_attempts: BTreeMap<(String, String), u32>,
    pub flow_lock_applications: BTreeMap<String, FlowLockApplication>,
    pub latest_flow_lock_application_index: Option<String>,
    pub latest_flow_lock_application_by_run: BTreeMap<String, String>,
    pub flow_lock_mode: Option<FlowLockMode>,
    pub flow_lock_mode_by_run: BTreeMap<String, FlowLockMode>,
    pub flow_lock_id_by_run: BTreeMap<String, String>,
    pub contract_hash_by_run: BTreeMap<String, String>,
}

impl RuntimeState {
    pub fn from_events(events: &[Event]) -> Self {
        let mut state = Self::default();
        for event in events {
            state.apply(event);
        }
        state
    }

    fn apply(&mut self, event: &Event) {
        match &event.payload {
            EventPayload::RunStarted {
                run_id,
                mode,
                activation_limit,
                stop_attempt_limit,
            } => {
                self.run_id = Some(run_id.clone());
                self.runs.insert(run_id.clone());
                self.boards.entry(run_id.clone()).or_default();
                self.board_versions.entry(run_id.clone()).or_insert(0);
                self.run_modes.insert(run_id.clone(), *mode);
                self.initial_activation_limits
                    .insert(run_id.clone(), *activation_limit);
                self.activation_limits
                    .insert(run_id.clone(), *activation_limit);
                self.stop_attempt_limits
                    .insert(run_id.clone(), *stop_attempt_limit);
                self.run_statuses
                    .entry(run_id.clone())
                    .or_insert(RunStatus::Ready);
            }
            EventPayload::RunActivationLimitChanged {
                run_id,
                activation_limit,
            } => {
                self.activation_limits
                    .insert(run_id.clone(), *activation_limit);
            }
            EventPayload::RunStatusChanged {
                run_id,
                status,
                reason,
            } => {
                self.runs.insert(run_id.clone());
                self.run_statuses.insert(run_id.clone(), *status);
                if let Some(reason) = reason {
                    self.run_status_reasons
                        .insert(run_id.clone(), reason.clone());
                } else {
                    self.run_status_reasons.remove(run_id);
                }
            }
            EventPayload::NodeActivated {
                run_id,
                activation_id,
                node_id,
                stable_key,
                activation_generation,
                trigger,
                context,
                stop_contract,
                flow_lock_mode,
                flow_lock_id,
                contract_hash,
            } => {
                self.activations.insert(
                    activation_key(run_id, activation_id),
                    Activation {
                        activation_id: activation_id.clone(),
                        run_id: run_id.clone(),
                        node_id: node_id.clone(),
                        stable_key: stable_key.clone(),
                        activation_generation: *activation_generation,
                        trigger: trigger.clone(),
                        status: ActivationStatus::Pending,
                        context: context.clone(),
                        stop_contract: stop_contract.clone(),
                        flow_lock_mode: *flow_lock_mode,
                        flow_lock_id: flow_lock_id.clone(),
                        contract_hash: contract_hash.clone(),
                    },
                );
            }
            EventPayload::ActivationStatusChanged {
                run_id,
                activation_id,
                status,
            } => {
                if let Some(activation) = self
                    .activations
                    .get_mut(&activation_key(run_id, activation_id))
                {
                    activation.status = *status;
                }
            }
            EventPayload::ParticipantExited {
                run_id,
                activation_id,
                ..
            } => {
                if let Some(activation) = self
                    .activations
                    .get_mut(&activation_key(run_id, activation_id))
                    && !matches!(
                        activation.status,
                        ActivationStatus::Blocked
                            | ActivationStatus::Completed
                            | ActivationStatus::Failed
                            | ActivationStatus::Cancelled
                    )
                {
                    activation.status = ActivationStatus::Failed;
                }
            }
            EventPayload::ArtifactDelivered {
                run_id,
                activation_id,
                artifact_id,
                artifact_key,
                content_hash,
                payload,
            } => {
                self.artifact_records.insert(
                    artifact_id.clone(),
                    ArtifactRecord {
                        artifact_id: artifact_id.clone(),
                        run_id: run_id.clone(),
                        activation_id: activation_id.clone(),
                        artifact_key: artifact_key.clone(),
                        content_hash: content_hash.clone(),
                        payload: payload.clone(),
                        event_sequence: event.sequence,
                    },
                );
                self.latest_artifact_by_slot_index
                    .insert(slot_index_key(run_id, artifact_key), artifact_id.clone());
                if let Some(activation) = self
                    .activations
                    .get_mut(&activation_key(run_id, activation_id))
                {
                    activation
                        .context
                        .insert(artifact_key.clone(), payload.clone());
                }
            }
            EventPayload::BoardPatched {
                run_id,
                key,
                value,
                version: _,
                ..
            } => {
                let board = self.boards.entry(run_id.clone()).or_default();
                board.insert(key.clone(), value.clone());
                self.board = board.clone();
                self.board_version = event.sequence;
                self.board_versions.insert(run_id.clone(), event.sequence);
                self.board_fact_versions
                    .insert(slot_index_key(run_id, key), event.sequence);
            }
            EventPayload::EffectRecorded {
                run_id,
                activation_id,
                effect_key,
                payload,
            } => {
                self.effects.insert(
                    effect_index_key(run_id, activation_id, effect_key),
                    payload.clone(),
                );
            }
            EventPayload::StopObserved {
                run_id,
                activation_id,
                observation,
            } => {
                self.stop_observations.insert(
                    stop_fact_id(run_id, activation_id, event.sequence),
                    observation.clone(),
                );
            }
            EventPayload::StopDecision {
                run_id,
                activation_id,
                decision,
            } => {
                self.stop_decisions.insert(
                    stop_fact_id(run_id, activation_id, event.sequence),
                    decision.clone(),
                );
                if decision.attempt > 0 {
                    self.stop_validation_attempts
                        .insert(activation_key(run_id, activation_id), decision.attempt);
                }
            }
            EventPayload::FlowApplied {
                run_id,
                mode,
                lock_id,
                content_hash,
            } => {
                self.apply_flow_update(event.sequence, run_id, *mode, lock_id, content_hash);
            }
            EventPayload::FlowUpdate {
                run_id,
                status: FlowUpdateStatus::Applied,
                mode,
                lock_id,
                contract_hash,
            } => {
                self.apply_flow_update(event.sequence, run_id, *mode, lock_id, contract_hash);
            }
            EventPayload::FlowUpdate { .. } => {}
        }
    }

    pub fn run_status(&self, run_id: &str) -> Option<RunStatus> {
        self.run_statuses.get(run_id).copied()
    }

    pub fn run_status_reason(&self, run_id: &str) -> Option<&str> {
        self.run_status_reasons.get(run_id).map(String::as_str)
    }

    pub fn run_mode(&self, run_id: &str) -> Option<RunMode> {
        self.run_modes.get(run_id).copied()
    }

    pub fn initial_activation_limit(&self, run_id: &str) -> Option<u64> {
        self.initial_activation_limits.get(run_id).copied()
    }

    pub fn activation_limit(&self, run_id: &str) -> Option<u64> {
        self.activation_limits.get(run_id).copied()
    }

    pub fn stop_attempt_limit(&self, run_id: &str) -> Option<u32> {
        self.stop_attempt_limits.get(run_id).copied()
    }

    pub fn activations_used(&self, run_id: &str) -> u64 {
        self.activations
            .values()
            .filter(|activation| activation.run_id == run_id)
            .count() as u64
    }

    pub fn artifact_fact_version(&self, run_id: &str, artifact_key: &str) -> Option<u64> {
        let artifact_id = self
            .latest_artifact_by_slot_index
            .get(&slot_index_key(run_id, artifact_key))?;
        self.artifact_records
            .get(artifact_id)
            .map(|artifact| artifact.event_sequence)
    }

    pub fn board_fact_version(&self, run_id: &str, key: &str) -> Option<u64> {
        self.board_fact_versions
            .get(&slot_index_key(run_id, key))
            .copied()
    }

    pub fn has_applied_trigger(&self, run_id: &str, trigger: &RouteTrigger) -> bool {
        self.activations.values().any(|activation| {
            activation.run_id == run_id && activation.trigger.as_ref() == Some(trigger)
        })
    }

    pub fn next_activation_generation(
        &self,
        run_id: &str,
        node_id: &str,
        stable_key: Option<&str>,
    ) -> u64 {
        self.activations
            .values()
            .filter(|activation| {
                activation.run_id == run_id
                    && activation.node_id == node_id
                    && activation.stable_key.as_deref() == stable_key
            })
            .map(|activation| activation.activation_generation)
            .max()
            .map_or(0, |generation| generation.saturating_add(1))
    }

    fn apply_flow_update(
        &mut self,
        event_sequence: u64,
        run_id: &str,
        mode: FlowLockMode,
        lock_id: &str,
        content_hash: &str,
    ) {
        let application_id = flow_lock_application_id(event_sequence);
        self.flow_lock_applications.insert(
            application_id.clone(),
            FlowLockApplication {
                application_id: application_id.clone(),
                run_id: run_id.to_owned(),
                mode,
                lock_id: lock_id.to_owned(),
                content_hash: content_hash.to_owned(),
                event_sequence,
            },
        );
        self.latest_flow_lock_application_index = Some(application_id.clone());
        self.latest_flow_lock_application_by_run
            .insert(run_id.to_owned(), application_id);
        self.flow_lock_mode = Some(mode);
        self.flow_lock_mode_by_run.insert(run_id.to_owned(), mode);
        self.flow_lock_id_by_run
            .insert(run_id.to_owned(), lock_id.to_owned());
        self.contract_hash_by_run
            .insert(run_id.to_owned(), content_hash.to_owned());
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct Runtime {
    store: LocalEventStore,
    state: RuntimeState,
}

impl Runtime {
    pub fn from_events(events: Vec<Event>) -> Self {
        Self {
            state: RuntimeState::from_events(&events),
            store: LocalEventStore { events },
        }
    }

    pub fn start_run(
        &mut self,
        run_id: impl Into<String>,
        nodes: Vec<NodeSpec>,
    ) -> Result<Vec<String>, RuntimeError> {
        self.start_run_with_options(run_id, nodes, RunMode::Finite, unbounded_activation_limit())
    }

    pub fn start_run_with_options(
        &mut self,
        run_id: impl Into<String>,
        nodes: Vec<NodeSpec>,
        mode: RunMode,
        activation_limit: u64,
    ) -> Result<Vec<String>, RuntimeError> {
        self.start_run_with_limits(
            run_id,
            nodes,
            mode,
            activation_limit,
            default_stop_attempt_limit(),
        )
    }

    pub fn start_run_with_limits(
        &mut self,
        run_id: impl Into<String>,
        nodes: Vec<NodeSpec>,
        mode: RunMode,
        activation_limit: u64,
        stop_attempt_limit: u32,
    ) -> Result<Vec<String>, RuntimeError> {
        let run_id = run_id.into();
        if self.state.runs.contains(&run_id) {
            return Err(RuntimeError::DuplicateRun { run_id });
        }

        if nodes.len() as u64 > activation_limit {
            return Err(RuntimeError::ActivationLimitExceeded {
                run_id,
                activation_limit,
                requested: nodes.len() as u64,
            });
        }

        let mut activation_ids = Vec::with_capacity(nodes.len());
        let mut seen_activation_ids = BTreeSet::new();
        for node in &nodes {
            let activation_id = activation_id_for(node, None, 0);
            if !seen_activation_ids.insert(activation_id.clone()) {
                return Err(RuntimeError::DuplicateActivation { activation_id });
            }
            activation_ids.push(activation_id);
        }

        self.append(EventPayload::RunStarted {
            run_id: run_id.clone(),
            mode,
            activation_limit,
            stop_attempt_limit,
        });

        for node in nodes {
            let activation_id = activation_id_for(&node, None, 0);
            self.append(EventPayload::NodeActivated {
                run_id: run_id.clone(),
                activation_id,
                node_id: node.id().to_owned(),
                stable_key: None,
                activation_generation: 0,
                trigger: None,
                context: BTreeMap::new(),
                stop_contract: node.stop_contract().clone(),
                flow_lock_mode: None,
                flow_lock_id: None,
                contract_hash: None,
            });
        }
        Ok(activation_ids)
    }

    pub fn deliver_artifact(
        &mut self,
        run_id: impl Into<String>,
        activation_id: impl Into<String>,
        artifact_key: impl Into<String>,
        payload: impl Into<String>,
    ) -> Result<String, RuntimeError> {
        let run_id = run_id.into();
        let activation_id = activation_id.into();
        let artifact_key = artifact_key.into();
        let artifact_key = ArtifactRef::new(artifact_key.clone())
            .map_err(|_| RuntimeError::InvalidFactKey {
                kind: "artifact",
                key: artifact_key,
            })?
            .key()
            .to_string();
        let payload = payload.into();
        self.require_activation_in_run(&run_id, &activation_id)?;
        let artifact_id = artifact_id(self.next_event_sequence());
        let content_hash = content_hash(&payload);
        self.append(EventPayload::ArtifactDelivered {
            run_id,
            activation_id,
            artifact_id: artifact_id.clone(),
            artifact_key,
            content_hash,
            payload,
        });
        Ok(artifact_id)
    }

    pub fn patch_board(
        &mut self,
        run_id: impl Into<String>,
        activation_id: impl Into<String>,
        patch: BoardPatch,
    ) -> Result<u64, RuntimeError> {
        let run_id = run_id.into();
        let activation_id = activation_id.into();
        self.require_activation_in_run(&run_id, &activation_id)?;
        let board_version = self.board_fact_version_for_key(&run_id, patch.key.as_str())?;
        if let Some(expected) = patch.expected_version
            && expected != board_version
        {
            return Err(RuntimeError::BoardVersionConflict {
                expected,
                actual: board_version,
            });
        }

        let version = self.next_event_sequence();
        self.append(EventPayload::BoardPatched {
            run_id,
            activation_id,
            key: patch.key.to_string(),
            value: patch.value,
            version,
        });
        Ok(version)
    }

    pub fn record_effect(
        &mut self,
        run_id: impl Into<String>,
        activation_id: impl Into<String>,
        effect_key: impl Into<String>,
        payload: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        let activation_id = activation_id.into();
        let run_id = run_id.into();
        self.require_activation_in_run(&run_id, &activation_id)?;
        self.append(EventPayload::EffectRecorded {
            run_id,
            activation_id,
            effect_key: effect_key.into(),
            payload: payload.into(),
        });
        Ok(())
    }

    pub fn activate_node(
        &mut self,
        run_id: impl Into<String>,
        node: &NodeSpec,
        stable_key: Option<&str>,
    ) -> Result<String, RuntimeError> {
        self.activate_node_with_context(run_id.into(), node, stable_key, BTreeMap::new())
    }

    pub fn fanout_from_artifact(
        &mut self,
        run_id: impl Into<String>,
        node: &NodeSpec,
        artifact_key: impl Into<String>,
    ) -> Result<Vec<String>, RuntimeError> {
        let run_id = run_id.into();
        self.require_run(&run_id)?;
        self.require_scheduling_enabled(&run_id, SchedulingIntent::Explicit)?;
        let artifact_key = artifact_key.into();
        let artifact_key = ArtifactRef::new(artifact_key.clone())
            .map_err(|_| RuntimeError::InvalidFactKey {
                kind: "artifact",
                key: artifact_key,
            })?
            .key()
            .to_string();
        if let Some(for_each_key) = node.for_each_key()
            && for_each_key != artifact_key
        {
            return Err(RuntimeError::ForEachMismatch {
                expected: for_each_key.to_owned(),
                actual: artifact_key,
            });
        }

        let artifact_id = self
            .state
            .latest_artifact_by_slot_index
            .get(&slot_index_key(&run_id, &artifact_key))
            .cloned()
            .ok_or_else(|| RuntimeError::ArtifactNotFound {
                artifact_key: artifact_key.clone(),
            })?;
        let artifact_payload = self
            .state
            .artifact_records
            .get(&artifact_id)
            .map(|artifact| artifact.payload.clone())
            .ok_or_else(|| RuntimeError::ArtifactNotFound {
                artifact_key: artifact_key.clone(),
            })?;

        let mut planned_activations = Vec::new();
        for (index, item) in artifact_payload.lines().enumerate() {
            let stable_key = format!("{artifact_key}/{index}");
            let (_, activation_id) =
                next_activation_identity(&self.state, &run_id, node.id(), Some(&stable_key));
            if self
                .state
                .activations
                .contains_key(&activation_key(&run_id, &activation_id))
            {
                return Err(RuntimeError::DuplicateActivation { activation_id });
            }
            let mut context = BTreeMap::new();
            context.insert("for_each".to_owned(), artifact_key.clone());
            context.insert("index".to_owned(), index.to_string());
            context.insert("item".to_owned(), item.to_owned());
            planned_activations.push((activation_id, stable_key, context));
        }

        self.require_activation_capacity(&run_id, planned_activations.len() as u64)?;

        let mut activation_ids = Vec::with_capacity(planned_activations.len());
        for (_, stable_key, context) in planned_activations {
            activation_ids.push(self.activate_node_with_context(
                run_id.clone(),
                node,
                Some(&stable_key),
                context,
            )?);
        }
        Ok(activation_ids)
    }

    fn apply_route_plan(
        &mut self,
        run_id: &str,
        node: &NodeSpec,
        trigger: &RouteTrigger,
        planned: &[PlannedActivationPreview],
    ) -> Result<Vec<String>, RuntimeError> {
        self.require_run(run_id)?;
        self.require_scheduling_enabled(run_id, SchedulingIntent::FactTriggeredRoute)?;
        if self.state.has_applied_trigger(run_id, trigger) {
            return Ok(Vec::new());
        }
        self.require_activation_capacity(run_id, planned.len() as u64)?;

        let flow_lock_mode = self.state.flow_lock_mode_by_run.get(run_id).copied();
        let flow_lock_id = self.state.flow_lock_id_by_run.get(run_id).cloned();
        let contract_hash = self.state.contract_hash_by_run.get(run_id).cloned();
        let mut payloads = Vec::with_capacity(planned.len());
        for activation in planned {
            let (generation, activation_id) = next_activation_identity(
                &self.state,
                run_id,
                node.id(),
                activation.stable_key.as_deref(),
            );
            if activation_id != activation.activation_id {
                return Err(RuntimeError::StaleRoutePlan {
                    activation_id: activation.activation_id.clone(),
                });
            }
            let mut context = BTreeMap::new();
            if let Some(index) = activation.index {
                context.insert("index".to_owned(), index.to_string());
            }
            if let Some(item) = &activation.item {
                context.insert("item".to_owned(), item.clone());
            }
            if let Some(stable_key) = &activation.stable_key {
                let for_each = stable_key.split('/').next().unwrap_or(stable_key);
                context.insert("for_each".to_owned(), for_each.to_owned());
            }
            payloads.push(EventPayload::NodeActivated {
                run_id: run_id.to_owned(),
                activation_id,
                node_id: node.id().to_owned(),
                stable_key: activation.stable_key.clone(),
                activation_generation: generation,
                trigger: Some(trigger.clone()),
                context,
                stop_contract: node.stop_contract().clone(),
                flow_lock_mode,
                flow_lock_id: flow_lock_id.clone(),
                contract_hash: contract_hash.clone(),
            });
        }

        let mut activation_ids = Vec::with_capacity(payloads.len());
        for payload in payloads {
            let EventPayload::NodeActivated { activation_id, .. } = &payload else {
                unreachable!("route plan payload must activate a node");
            };
            activation_ids.push(activation_id.clone());
            self.append(payload);
        }
        Ok(activation_ids)
    }

    pub fn validate_stop(
        &self,
        run_id: impl AsRef<str>,
        activation_id: impl AsRef<str>,
    ) -> Result<(), StopValidationError> {
        let run_id = run_id.as_ref();
        let activation_id = activation_id.as_ref();
        if !self.state.runs.contains(run_id) {
            return Err(StopValidationError::RunNotFound {
                run_id: run_id.to_owned(),
            });
        }
        let activation = self
            .state
            .activations
            .get(&activation_key(run_id, activation_id))
            .ok_or_else(|| StopValidationError::ActivationNotFoundInRun {
                run_id: run_id.to_owned(),
                activation_id: activation_id.to_owned(),
            })?;

        for artifact_key in activation.stop_contract.required_artifacts() {
            if !activation.context.contains_key(artifact_key) {
                return Err(StopValidationError::MissingArtifact {
                    activation_id: activation_id.to_owned(),
                    artifact_key: artifact_key.clone(),
                });
            }
        }

        for effect_key in activation.stop_contract.required_effects() {
            if !self.state.effects.contains_key(&effect_index_key(
                run_id,
                activation_id,
                effect_key,
            )) {
                return Err(StopValidationError::MissingEffect {
                    activation_id: activation_id.to_owned(),
                    effect_key: effect_key.clone(),
                });
            }
        }

        Ok(())
    }

    pub fn apply_flow_lock(
        &mut self,
        run_id: impl Into<String>,
        mode: FlowLockMode,
        lock_id: impl Into<String>,
        content_hash: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        let run_id = run_id.into();
        self.require_run(&run_id)?;
        let lock_id = lock_id.into();
        let contract_hash = content_hash.into();
        for status in [
            FlowUpdateStatus::Proposed,
            FlowUpdateStatus::Checked,
            FlowUpdateStatus::Applied,
        ] {
            self.append(EventPayload::FlowUpdate {
                run_id: run_id.clone(),
                status,
                mode,
                lock_id: lock_id.clone(),
                contract_hash: contract_hash.clone(),
            });
        }
        Ok(())
    }

    pub fn append_legacy_flow_applied(
        &mut self,
        run_id: impl Into<String>,
        mode: FlowLockMode,
        lock_id: impl Into<String>,
        content_hash: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        let run_id = run_id.into();
        self.require_run(&run_id)?;
        self.append(EventPayload::FlowApplied {
            run_id,
            mode,
            lock_id: lock_id.into(),
            content_hash: content_hash.into(),
        });
        Ok(())
    }

    pub fn set_run_status(
        &mut self,
        run_id: impl Into<String>,
        status: RunStatus,
    ) -> Result<(), RuntimeError> {
        self.set_run_status_with_reason(run_id, status, None)
    }

    pub fn set_run_status_with_reason(
        &mut self,
        run_id: impl Into<String>,
        status: RunStatus,
        reason: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let run_id = run_id.into();
        self.require_run(&run_id)?;
        if self.state.run_status(&run_id) != Some(status)
            || self.state.run_status_reason(&run_id) != reason
        {
            self.append(EventPayload::RunStatusChanged {
                run_id,
                status,
                reason: reason.map(str::to_owned),
            });
        }
        Ok(())
    }

    pub fn raise_activation_limit(
        &mut self,
        run_id: impl Into<String>,
        activation_limit: u64,
    ) -> Result<(), RuntimeError> {
        let run_id = run_id.into();
        self.require_run(&run_id)?;
        let current = self
            .state
            .activation_limit(&run_id)
            .unwrap_or(unbounded_activation_limit());
        if activation_limit < current {
            return Err(RuntimeError::ActivationLimitDecrease {
                run_id,
                current,
                requested: activation_limit,
            });
        }
        if activation_limit > current {
            self.append(EventPayload::RunActivationLimitChanged {
                run_id,
                activation_limit,
            });
        }
        Ok(())
    }

    pub fn state(&self) -> &RuntimeState {
        &self.state
    }

    pub fn events(&self) -> &[Event] {
        self.store.replay()
    }

    pub fn has_run(&self, run_id: &str) -> bool {
        self.state.runs.contains(run_id)
    }

    fn append(&mut self, payload: EventPayload) -> Event {
        let event = self.store.append(payload);
        self.state.apply(&event);
        event
    }

    fn next_event_sequence(&self) -> u64 {
        self.store.replay().len() as u64 + 1
    }

    fn activate_node_with_context(
        &mut self,
        run_id: String,
        node: &NodeSpec,
        stable_key: Option<&str>,
        context: BTreeMap<String, String>,
    ) -> Result<String, RuntimeError> {
        self.require_run(&run_id)?;
        self.require_scheduling_enabled(&run_id, SchedulingIntent::Explicit)?;
        let (activation_generation, activation_id) =
            next_activation_identity(&self.state, &run_id, node.id(), stable_key);
        if self
            .state
            .activations
            .contains_key(&activation_key(&run_id, &activation_id))
        {
            return Err(RuntimeError::DuplicateActivation { activation_id });
        }
        self.require_activation_capacity(&run_id, 1)?;
        let flow_lock_mode = self.state.flow_lock_mode_by_run.get(&run_id).copied();
        let flow_lock_id = self.state.flow_lock_id_by_run.get(&run_id).cloned();
        let contract_hash = self.state.contract_hash_by_run.get(&run_id).cloned();

        self.append(EventPayload::NodeActivated {
            run_id,
            activation_id: activation_id.clone(),
            node_id: node.id().to_owned(),
            stable_key: stable_key.map(str::to_owned),
            activation_generation,
            trigger: None,
            context,
            stop_contract: node.stop_contract().clone(),
            flow_lock_mode,
            flow_lock_id,
            contract_hash,
        });

        Ok(activation_id)
    }

    fn require_run(&self, run_id: &str) -> Result<(), RuntimeError> {
        if self.state.runs.contains(run_id) {
            Ok(())
        } else {
            Err(RuntimeError::RunNotFound {
                run_id: run_id.to_owned(),
            })
        }
    }

    fn require_activation_in_run(
        &self,
        run_id: &str,
        activation_id: &str,
    ) -> Result<(), RuntimeError> {
        self.require_run(run_id)?;
        if self
            .state
            .activations
            .contains_key(&activation_key(run_id, activation_id))
        {
            Ok(())
        } else {
            Err(RuntimeError::ActivationNotFoundInRun {
                run_id: run_id.to_owned(),
                activation_id: activation_id.to_owned(),
            })
        }
    }

    fn board_fact_version_for_key(&self, run_id: &str, key: &str) -> Result<u64, RuntimeError> {
        self.require_run(run_id)?;
        Ok(self.state.board_fact_version(run_id, key).unwrap_or(0))
    }

    fn require_activation_capacity(
        &self,
        run_id: &str,
        requested: u64,
    ) -> Result<(), RuntimeError> {
        let activation_limit = self
            .state
            .activation_limit(run_id)
            .unwrap_or(unbounded_activation_limit());
        let used = self.state.activations_used(run_id);
        if requested > activation_limit.saturating_sub(used) {
            return Err(RuntimeError::ActivationLimitExceeded {
                run_id: run_id.to_owned(),
                activation_limit,
                requested,
            });
        }
        Ok(())
    }

    fn require_scheduling_enabled(
        &self,
        run_id: &str,
        intent: SchedulingIntent,
    ) -> Result<(), RuntimeError> {
        let status = self
            .state
            .run_status(run_id)
            .ok_or_else(|| RuntimeError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
        if scheduling_enabled(&self.state, run_id, intent) {
            return Ok(());
        }
        if status == RunStatus::Paused {
            return Err(RuntimeError::RunPaused {
                run_id: run_id.to_owned(),
            });
        }
        let action = match intent {
            SchedulingIntent::Explicit => "schedule an explicit activation",
            SchedulingIntent::FactTriggeredRoute => "apply a fact-triggered route",
        };
        Err(RuntimeError::InvalidRunStatusTransition {
            run_id: run_id.to_owned(),
            action: action.to_string(),
            status,
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BoardPatch {
    key: FactKey,
    value: String,
    expected_version: Option<u64>,
}

impl BoardPatch {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Result<Self, FactError> {
        Ok(Self {
            key: FactKey::new(key)?,
            value: value.into(),
            expected_version: None,
        })
    }

    pub fn expect_version(mut self, version: u64) -> Self {
        self.expected_version = Some(version);
        self
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RuntimeError {
    ActivationLimitDecrease {
        run_id: String,
        current: u64,
        requested: u64,
    },
    ActivationLimitExceeded {
        run_id: String,
        activation_limit: u64,
        requested: u64,
    },
    ActivationLimitIncreaseRequired {
        run_id: String,
        current: u64,
    },
    ActivationNotFound {
        activation_id: String,
    },
    ActivationNotFoundInRun {
        run_id: String,
        activation_id: String,
    },
    ArtifactNotFound {
        artifact_key: String,
    },
    InvalidFactKey {
        kind: &'static str,
        key: String,
    },
    BoardVersionConflict {
        expected: u64,
        actual: u64,
    },
    DuplicateActivation {
        activation_id: String,
    },
    DuplicateRun {
        run_id: String,
    },
    ParticipantExitConflict {
        run_id: String,
        activation_id: String,
        allocation_generation: u64,
    },
    ForEachMismatch {
        expected: String,
        actual: String,
    },
    RunNotFound {
        run_id: String,
    },
    RunPaused {
        run_id: String,
    },
    RunConfigurationConflict {
        run_id: String,
        expected_mode: RunMode,
        actual_mode: RunMode,
        expected_activation_limit: u64,
        actual_activation_limit: u64,
    },
    InvalidRunStatusTransition {
        run_id: String,
        action: String,
        status: RunStatus,
    },
    RunModeDoesNotAllowControl {
        run_id: String,
        action: String,
        mode: RunMode,
    },
    RunNotQuiescent {
        run_id: String,
        status: RunStatus,
    },
    StaleRoutePlan {
        activation_id: String,
    },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::ActivationLimitDecrease {
                run_id,
                current,
                requested,
            } => write!(
                formatter,
                "activation limit for run {run_id} cannot decrease from {current} to {requested}"
            ),
            RuntimeError::ActivationLimitExceeded {
                run_id,
                activation_limit,
                requested,
            } => write!(
                formatter,
                "activation limit exceeded for run {run_id}: limit {activation_limit}, requested {requested}"
            ),
            RuntimeError::ActivationLimitIncreaseRequired { run_id, current } => write!(
                formatter,
                "run {run_id} requires an activation limit greater than {current} to resume"
            ),
            RuntimeError::ActivationNotFound { activation_id } => {
                write!(formatter, "activation not found: {activation_id}")
            }
            RuntimeError::ActivationNotFoundInRun {
                run_id,
                activation_id,
            } => {
                write!(
                    formatter,
                    "activation not found in run {run_id}: {activation_id}"
                )
            }
            RuntimeError::ArtifactNotFound { artifact_key } => {
                write!(formatter, "artifact not found: {artifact_key}")
            }
            RuntimeError::InvalidFactKey { kind, key } => {
                write!(formatter, "invalid {kind} fact key: {key}")
            }
            RuntimeError::BoardVersionConflict { expected, actual } => write!(
                formatter,
                "board version conflict: expected {expected}, actual {actual}"
            ),
            RuntimeError::DuplicateActivation { activation_id } => {
                write!(formatter, "duplicate activation: {activation_id}")
            }
            RuntimeError::DuplicateRun { run_id } => write!(formatter, "duplicate run: {run_id}"),
            RuntimeError::ParticipantExitConflict {
                run_id,
                activation_id,
                allocation_generation,
            } => write!(
                formatter,
                "participant exit conflicts for run {run_id} activation {activation_id} allocation {allocation_generation}"
            ),
            RuntimeError::ForEachMismatch { expected, actual } => {
                write!(
                    formatter,
                    "for_each mismatch: expected {expected}, actual {actual}"
                )
            }
            RuntimeError::RunNotFound { run_id } => write!(formatter, "run not found: {run_id}"),
            RuntimeError::RunPaused { run_id } => write!(formatter, "run {run_id} is paused"),
            RuntimeError::RunConfigurationConflict {
                run_id,
                expected_mode,
                actual_mode,
                expected_activation_limit,
                actual_activation_limit,
            } => write!(
                formatter,
                "run {run_id} configuration conflict: expected {expected_mode:?}/{expected_activation_limit}, got {actual_mode:?}/{actual_activation_limit}"
            ),
            RuntimeError::InvalidRunStatusTransition {
                run_id,
                action,
                status,
            } => write!(
                formatter,
                "run {run_id} cannot {action} from status {status:?}"
            ),
            RuntimeError::RunModeDoesNotAllowControl {
                run_id,
                action,
                mode,
            } => write!(
                formatter,
                "run {run_id} in mode {mode:?} does not allow {action}"
            ),
            RuntimeError::RunNotQuiescent { run_id, status } => write!(
                formatter,
                "run {run_id} must be quiescent before completion, current status {status:?}"
            ),
            RuntimeError::StaleRoutePlan { activation_id } => {
                write!(formatter, "stale route plan for activation {activation_id}")
            }
        }
    }
}

impl Error for RuntimeError {}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum StopValidationError {
    RunNotFound {
        run_id: String,
    },
    ActivationNotFound {
        activation_id: String,
    },
    ActivationNotFoundInRun {
        run_id: String,
        activation_id: String,
    },
    MissingArtifact {
        activation_id: String,
        artifact_key: String,
    },
    MissingEffect {
        activation_id: String,
        effect_key: String,
    },
}

impl fmt::Display for StopValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopValidationError::RunNotFound { run_id } => {
                write!(formatter, "run not found: {run_id}")
            }
            StopValidationError::ActivationNotFound { activation_id } => {
                write!(formatter, "activation not found: {activation_id}")
            }
            StopValidationError::ActivationNotFoundInRun {
                run_id,
                activation_id,
            } => {
                write!(
                    formatter,
                    "activation not found in run {run_id}: {activation_id}"
                )
            }
            StopValidationError::MissingArtifact {
                activation_id,
                artifact_key,
            } => write!(
                formatter,
                "activation {activation_id} is missing artifact {artifact_key}"
            ),
            StopValidationError::MissingEffect {
                activation_id,
                effect_key,
            } => write!(
                formatter,
                "activation {activation_id} is missing effect {effect_key}"
            ),
        }
    }
}

impl Error for StopValidationError {}
