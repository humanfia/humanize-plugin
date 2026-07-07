use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Event {
    pub sequence: u64,
    pub payload: EventPayload,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum EventPayload {
    RunStarted {
        run_id: String,
    },
    NodeActivated {
        run_id: String,
        activation_id: String,
        node_id: String,
        stable_key: Option<String>,
        context: BTreeMap<String, String>,
        stop_contract: StopContract,
        flow_lock_mode: Option<FlowLockMode>,
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
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FlowLockMode {
    FutureActivations,
    CheckpointRestart,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct LocalEventStore {
    events: Vec<Event>,
}

impl LocalEventStore {
    pub fn append(&mut self, payload: EventPayload) -> Event {
        let event = Event {
            sequence: self.events.len() as u64 + 1,
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
pub enum ActivationStatus {
    #[default]
    Active,
    Stopped,
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
}

impl Default for Activation {
    fn default() -> Self {
        Self {
            activation_id: String::new(),
            run_id: String::new(),
            node_id: String::new(),
            stable_key: None,
            status: ActivationStatus::Active,
            context: BTreeMap::new(),
            stop_contract: StopContract::default(),
            flow_lock_mode: None,
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
    pub activations: BTreeMap<(String, String), Activation>,
    pub effects: BTreeMap<(String, String, String), String>,
    pub flow_lock_applications: BTreeMap<String, FlowLockApplication>,
    pub latest_flow_lock_application_index: Option<String>,
    pub latest_flow_lock_application_by_run: BTreeMap<String, String>,
    pub flow_lock_mode: Option<FlowLockMode>,
    pub flow_lock_mode_by_run: BTreeMap<String, FlowLockMode>,
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
            }
            EventPayload::NodeActivated {
                run_id,
                activation_id,
                node_id,
                stable_key,
                context,
                stop_contract,
                flow_lock_mode,
            } => {
                self.activations.insert(
                    activation_key(run_id, activation_id),
                    Activation {
                        activation_id: activation_id.clone(),
                        run_id: run_id.clone(),
                        node_id: node_id.clone(),
                        stable_key: stable_key.clone(),
                        status: ActivationStatus::Active,
                        context: context.clone(),
                        stop_contract: stop_contract.clone(),
                        flow_lock_mode: *flow_lock_mode,
                    },
                );
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
            EventPayload::FlowApplied {
                run_id,
                mode,
                lock_id,
                content_hash,
            } => {
                let application_id = flow_lock_application_id(event.sequence);
                self.flow_lock_applications.insert(
                    application_id.clone(),
                    FlowLockApplication {
                        application_id: application_id.clone(),
                        run_id: run_id.clone(),
                        mode: *mode,
                        lock_id: lock_id.clone(),
                        content_hash: content_hash.clone(),
                        event_sequence: event.sequence,
                    },
                );
                self.latest_flow_lock_application_index = Some(application_id.clone());
                self.latest_flow_lock_application_by_run
                    .insert(run_id.clone(), application_id);
                self.flow_lock_mode = Some(*mode);
                self.flow_lock_mode_by_run.insert(run_id.clone(), *mode);
            }
        }
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

        let mut activation_ids = Vec::new();
        for (index, item) in artifact_payload.lines().enumerate() {
            let stable_key = format!("{artifact_key}/{index}");
            let mut context = BTreeMap::new();
            context.insert("for_each".to_owned(), artifact_key.clone());
            context.insert("index".to_owned(), index.to_string());
            context.insert("item".to_owned(), item.to_owned());
            activation_ids.push(self.activate_node_with_context(
                run_id.clone(),
                node,
                Some(&stable_key),
                context,
            )?);
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

        self.append(EventPayload::NodeActivated {
            run_id,
            activation_id: activation_id.clone(),
            node_id: node.id().to_owned(),
            stable_key: stable_key.map(str::to_owned),
            context,
            stop_contract: node.stop_contract().clone(),
            flow_lock_mode,
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
