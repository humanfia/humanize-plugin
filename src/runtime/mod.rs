use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

mod driver;
mod route_preview;

pub use driver::{
    ControlCommand, DriverRender, DriverState, DriverTickInput, DriverTickReport, LoopBudget,
    RunCompletionMode, TickBudget,
};
pub use route_preview::{PlannedActivationPreview, RoutePreview, preview_flow_routes};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Event {
    pub sequence: u64,
    pub source: EventSource,
    pub kind: EventKind,
    pub strength: EventStrength,
    pub actor: Option<String>,
    pub correlation: Option<String>,
    pub payload: EventPayload,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct EventSource {
    pub run_id: Option<String>,
    pub activation_id: Option<String>,
    pub source_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EventKind {
    ActivationStatusChanged,
    ArtifactDelivered,
    BoardPatched,
    EffectRecorded,
    FlowApplied,
    FlowUpdate,
    NodeActivated,
    RunStarted,
    RunStatusChanged,
    StopDecision,
    StopObserved,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EventStrength {
    Applied,
    Checked,
    Decision,
    Observed,
    Proposed,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum EventPayload {
    RunStarted {
        run_id: String,
    },
    RunStatusChanged {
        run_id: String,
        status: RunStatus,
    },
    NodeActivated {
        run_id: String,
        activation_id: String,
        node_id: String,
        stable_key: Option<String>,
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
            EventPayload::RunStarted { run_id }
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
            EventPayload::RunStatusChanged { .. } => EventKind::RunStatusChanged,
            EventPayload::NodeActivated { .. } => EventKind::NodeActivated,
            EventPayload::ActivationStatusChanged { .. } => EventKind::ActivationStatusChanged,
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FlowLockMode {
    FutureActivations,
    CheckpointRestart,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FlowUpdateStatus {
    Proposed,
    Checked,
    Applied,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StopObservation {
    pub reason: String,
}

impl StopObservation {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StopDecisionKind {
    Allow,
    Deny,
    Block,
    Yield,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StopDecision {
    pub kind: StopDecisionKind,
    pub attempt: u32,
    pub missing_artifacts: Vec<String>,
    pub missing_effects: Vec<String>,
    pub reason: Option<String>,
}

impl StopDecision {
    pub fn allow(attempt: u32) -> Self {
        Self {
            kind: StopDecisionKind::Allow,
            attempt,
            missing_artifacts: Vec::new(),
            missing_effects: Vec::new(),
            reason: None,
        }
    }

    pub fn deny_until_limit(
        attempt: u32,
        missing_artifacts: Vec<String>,
        missing_effects: Vec<String>,
    ) -> Self {
        Self {
            kind: StopDecisionKind::Deny,
            attempt,
            missing_artifacts,
            missing_effects,
            reason: Some("missing stop requirements".into()),
        }
    }

    pub fn block(
        attempt: u32,
        missing_artifacts: Vec<String>,
        missing_effects: Vec<String>,
    ) -> Self {
        Self {
            kind: StopDecisionKind::Block,
            attempt,
            missing_artifacts,
            missing_effects,
            reason: Some("stop validation limit reached".into()),
        }
    }

    pub fn yield_now(attempt: u32, reason: impl Into<String>) -> Self {
        Self {
            kind: StopDecisionKind::Yield,
            attempt,
            missing_artifacts: Vec::new(),
            missing_effects: Vec::new(),
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
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

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct NodeSpec {
    id: String,
    stop_contract: StopContract,
    for_each: Option<String>,
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

    pub fn with_for_each(mut self, artifact_key: impl Into<String>) -> Self {
        self.for_each = Some(artifact_key.into());
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn stop_contract(&self) -> &StopContract {
        &self.stop_contract
    }

    pub fn for_each_key(&self) -> Option<&str> {
        self.for_each.as_deref()
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
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

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
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

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
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
    pub run_statuses: BTreeMap<String, RunStatus>,
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
            EventPayload::RunStarted { run_id } => {
                self.run_id = Some(run_id.clone());
                self.runs.insert(run_id.clone());
                self.boards.entry(run_id.clone()).or_default();
                self.board_versions.entry(run_id.clone()).or_insert(0);
                self.run_statuses
                    .entry(run_id.clone())
                    .or_insert(RunStatus::Ready);
            }
            EventPayload::RunStatusChanged { run_id, status } => {
                self.runs.insert(run_id.clone());
                self.run_statuses.insert(run_id.clone(), *status);
            }
            EventPayload::NodeActivated {
                run_id,
                activation_id,
                node_id,
                stable_key,
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
                version,
                ..
            } => {
                let board = self.boards.entry(run_id.clone()).or_default();
                board.insert(key.clone(), value.clone());
                self.board = board.clone();
                self.board_version = *version;
                self.board_versions.insert(run_id.clone(), *version);
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
    pub fn start_run(
        &mut self,
        run_id: impl Into<String>,
        nodes: Vec<NodeSpec>,
    ) -> Result<Vec<String>, RuntimeError> {
        let run_id = run_id.into();
        if self.state.runs.contains(&run_id) {
            return Err(RuntimeError::DuplicateRun { run_id });
        }

        let mut activation_ids = Vec::with_capacity(nodes.len());
        let mut seen_activation_ids = BTreeSet::new();
        for node in &nodes {
            let activation_id = activation_id_for(node, None);
            if !seen_activation_ids.insert(activation_id.clone()) {
                return Err(RuntimeError::DuplicateActivation { activation_id });
            }
            activation_ids.push(activation_id);
        }

        self.append(EventPayload::RunStarted {
            run_id: run_id.clone(),
        });

        for node in nodes {
            self.activate_node(run_id.as_str(), &node, None)?;
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
        let board_version = self.board_version_for_run(&run_id)?;
        if let Some(expected) = patch.expected_version {
            if expected != board_version {
                return Err(RuntimeError::BoardVersionConflict {
                    expected,
                    actual: board_version,
                });
            }
        }

        let version = board_version + 1;
        self.append(EventPayload::BoardPatched {
            run_id,
            activation_id,
            key: patch.key,
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
        let artifact_key = artifact_key.into();
        if let Some(for_each_key) = node.for_each_key() {
            if for_each_key != artifact_key {
                return Err(RuntimeError::ForEachMismatch {
                    expected: for_each_key.to_owned(),
                    actual: artifact_key,
                });
            }
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
            let activation_id = activation_id_for(node, Some(&stable_key));
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

        let mut activation_ids = Vec::with_capacity(planned_activations.len());
        for (activation_id, stable_key, context) in planned_activations {
            self.activate_node_with_context(run_id.clone(), node, Some(&stable_key), context)?;
            activation_ids.push(activation_id);
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
        let activation_id = activation_id_for(node, stable_key);
        if self
            .state
            .activations
            .contains_key(&activation_key(&run_id, &activation_id))
        {
            return Err(RuntimeError::DuplicateActivation { activation_id });
        }
        let flow_lock_mode = self.state.flow_lock_mode_by_run.get(&run_id).copied();
        let flow_lock_id = self.state.flow_lock_id_by_run.get(&run_id).cloned();
        let contract_hash = self.state.contract_hash_by_run.get(&run_id).cloned();

        self.append(EventPayload::NodeActivated {
            run_id,
            activation_id: activation_id.clone(),
            node_id: node.id().to_owned(),
            stable_key: stable_key.map(str::to_owned),
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

    fn board_version_for_run(&self, run_id: &str) -> Result<u64, RuntimeError> {
        self.require_run(run_id)?;
        Ok(self.state.board_versions.get(run_id).copied().unwrap_or(0))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BoardPatch {
    key: String,
    value: String,
    expected_version: Option<u64>,
}

impl BoardPatch {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            expected_version: None,
        }
    }

    pub fn expect_version(mut self, version: u64) -> Self {
        self.expected_version = Some(version);
        self
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RuntimeError {
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
    ForEachMismatch {
        expected: String,
        actual: String,
    },
    RunNotFound {
        run_id: String,
    },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            RuntimeError::BoardVersionConflict { expected, actual } => write!(
                formatter,
                "board version conflict: expected {expected}, actual {actual}"
            ),
            RuntimeError::DuplicateActivation { activation_id } => {
                write!(formatter, "duplicate activation: {activation_id}")
            }
            RuntimeError::DuplicateRun { run_id } => write!(formatter, "duplicate run: {run_id}"),
            RuntimeError::ForEachMismatch { expected, actual } => {
                write!(
                    formatter,
                    "for_each mismatch: expected {expected}, actual {actual}"
                )
            }
            RuntimeError::RunNotFound { run_id } => write!(formatter, "run not found: {run_id}"),
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

fn activation_id_for(node: &NodeSpec, stable_key: Option<&str>) -> String {
    match stable_key {
        Some(stable_key) => format!("{}:{stable_key}", node.id()),
        None => node.id().to_owned(),
    }
}

fn activation_key(run_id: &str, activation_id: &str) -> (String, String) {
    (run_id.to_owned(), activation_id.to_owned())
}

fn slot_index_key(run_id: &str, artifact_key: &str) -> (String, String) {
    (run_id.to_owned(), artifact_key.to_owned())
}

fn effect_index_key(
    run_id: &str,
    activation_id: &str,
    effect_key: &str,
) -> (String, String, String) {
    (
        run_id.to_owned(),
        activation_id.to_owned(),
        effect_key.to_owned(),
    )
}

fn stop_fact_id(run_id: &str, activation_id: &str, event_sequence: u64) -> String {
    format!("{run_id}/{activation_id}/{event_sequence}")
}

fn artifact_id(event_sequence: u64) -> String {
    format!("artifact:{event_sequence}")
}

fn flow_lock_application_id(event_sequence: u64) -> String {
    format!("flow-lock-application:{event_sequence}")
}

fn content_hash(payload: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in payload.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}
