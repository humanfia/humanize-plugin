use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PublicEventKind {
    RunStarted,
    RunStatus,
    RunCompleted,
    FlowRevisionPrepared,
    FlowRevisionApplied,
    FlowRevisionRejected,
    FactRecorded,
    ArtifactRecorded,
    RouteDecided,
    ActivationCreated,
    ActivationStatus,
    ActivationCompleted,
    AgentSessionStarted,
    AgentSessionBound,
    AgentSessionEnded,
    HookObserved,
    ContextCompactionStarted,
    ContextCompactionFinished,
    WorkProfileObserved,
    QosObserved,
    QosApplied,
    UsageObserved,
    MachineInputDelivered,
    StopObserved,
    StopDecided,
}

impl PublicEventKind {
    pub(crate) const ALL: &'static [Self] = &[
        Self::RunStarted,
        Self::RunStatus,
        Self::RunCompleted,
        Self::FlowRevisionPrepared,
        Self::FlowRevisionApplied,
        Self::FlowRevisionRejected,
        Self::FactRecorded,
        Self::ArtifactRecorded,
        Self::RouteDecided,
        Self::ActivationCreated,
        Self::ActivationStatus,
        Self::ActivationCompleted,
        Self::AgentSessionStarted,
        Self::AgentSessionBound,
        Self::AgentSessionEnded,
        Self::HookObserved,
        Self::ContextCompactionStarted,
        Self::ContextCompactionFinished,
        Self::WorkProfileObserved,
        Self::QosObserved,
        Self::QosApplied,
        Self::UsageObserved,
        Self::MachineInputDelivered,
        Self::StopObserved,
        Self::StopDecided,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RunStarted => "run.started",
            Self::RunStatus => "run.status",
            Self::RunCompleted => "run.completed",
            Self::FlowRevisionPrepared => "flow_revision.prepared",
            Self::FlowRevisionApplied => "flow_revision.applied",
            Self::FlowRevisionRejected => "flow_revision.rejected",
            Self::FactRecorded => "fact.recorded",
            Self::ArtifactRecorded => "artifact.recorded",
            Self::RouteDecided => "route.decided",
            Self::ActivationCreated => "activation.created",
            Self::ActivationStatus => "activation.status",
            Self::ActivationCompleted => "activation.completed",
            Self::AgentSessionStarted => "agent_session.started",
            Self::AgentSessionBound => "agent_session.bound",
            Self::AgentSessionEnded => "agent_session.ended",
            Self::HookObserved => "hook.observed",
            Self::ContextCompactionStarted => "context_compaction.started",
            Self::ContextCompactionFinished => "context_compaction.finished",
            Self::WorkProfileObserved => "work_profile.observed",
            Self::QosObserved => "qos.observed",
            Self::QosApplied => "qos.applied",
            Self::UsageObserved => "usage.observed",
            Self::MachineInputDelivered => "machine_input.delivered",
            Self::StopObserved => "stop.observed",
            Self::StopDecided => "stop.decided",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PublicSource {
    Driver,
    Hook,
    MachineInput,
    RunAssets,
    Runtime,
}

impl PublicSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Driver => "driver",
            Self::Hook => "hook",
            Self::MachineInput => "machine_input",
            Self::RunAssets => "run_assets",
            Self::Runtime => "runtime",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PublicContentRef {
    pub(crate) sha256: String,
    pub(crate) content_ref: String,
    pub(crate) length: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
}

impl PublicContentRef {
    pub(crate) fn hashed(sha256: String, length: u64) -> Self {
        Self {
            content_ref: sha256.clone(),
            sha256,
            length,
            path: None,
        }
    }

    pub(crate) fn file(sha256: String, length: u64, path: String) -> Self {
        Self {
            content_ref: sha256.clone(),
            sha256,
            length,
            path: Some(path),
        }
    }

    fn to_value(&self) -> Value {
        let mut value = json!({
            "sha256": self.sha256,
            "ref": self.content_ref,
            "length": self.length,
        });
        if let Some(path) = self.path.as_ref() {
            value["path"] = Value::String(path.clone());
        }
        value
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunTransition {
    Started,
    Status,
    Completed,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct RunLifecyclePayload {
    pub(crate) transition: RunTransition,
    pub(crate) mode: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) reason: Option<PublicContentRef>,
    pub(crate) activation_limit: Option<u64>,
    pub(crate) stop_attempt_limit: Option<u64>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RevisionState {
    Prepared,
    Applied,
    Rejected,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct RevisionLifecyclePayload {
    pub(crate) state: RevisionState,
    pub(crate) flow_id: String,
    pub(crate) content: PublicContentRef,
    pub(crate) review_status: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FactKind {
    Board,
    Effect,
    Explicit,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct FactPayload {
    pub(crate) fact_kind: FactKind,
    pub(crate) key: String,
    pub(crate) version: Option<u64>,
    pub(crate) content: PublicContentRef,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ArtifactPayload {
    pub(crate) artifact_id: String,
    pub(crate) artifact_key: String,
    pub(crate) content: PublicContentRef,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct RoutePayload {
    pub(crate) flow_id: String,
    pub(crate) route_id: String,
    pub(crate) route_index: u64,
    pub(crate) predicate: String,
    pub(crate) for_each: Option<String>,
    pub(crate) source_artifact_id: Option<String>,
    pub(crate) trigger_fact: String,
    pub(crate) trigger_version: u64,
    pub(crate) planned_activation_ids: Vec<String>,
    pub(crate) applied_activation_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivationState {
    Planned,
    Starting,
    Running,
    Ready,
    Suspended,
    WaitingForStop,
    ValidatingStop,
    Blocked,
    Completed,
    Failed,
    Cancelled,
    Closed,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ActivationLifecyclePayload {
    pub(crate) state: ActivationState,
    pub(crate) node_id: Option<String>,
    pub(crate) allocation_generation: Option<u64>,
    pub(crate) termination: Option<PublicContentRef>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionState {
    Started,
    Bound,
    Ended,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativePlatform {
    Claude,
    Codex,
    Other,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct SessionLifecyclePayload {
    pub(crate) state: SessionState,
    pub(crate) platform: NativePlatform,
    pub(crate) exit_status: Option<i32>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HookKind {
    AgentReady,
    AgentReadyFailure,
    TmuxGuardBlocked,
    Other,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct HookPayload {
    pub(crate) hook_kind: HookKind,
    pub(crate) detail: PublicContentRef,
    pub(crate) status: Option<String>,
    pub(crate) allocation_generation: Option<u64>,
    pub(crate) context_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompactionState {
    Started,
    Finished,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct CompactionPayload {
    pub(crate) state: CompactionState,
    pub(crate) context_generation: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkProfilePayload {
    pub(crate) intent: String,
    pub(crate) workspace_access: String,
    pub(crate) tool_execution: String,
    pub(crate) network_access: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QosState {
    Observed,
    Applied,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct QosPayload {
    pub(crate) state: QosState,
    pub(crate) urgency: String,
    pub(crate) completion_target: Option<PublicContentRef>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UsageMetric {
    InputTokens,
    OutputTokens,
    TotalTokens,
    WallTimeMs,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct UsagePayload {
    pub(crate) metric: UsageMetric,
    pub(crate) value: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct MachineInputPayload {
    pub(crate) status: String,
    pub(crate) content: PublicContentRef,
    pub(crate) submit_key_count: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StopStage {
    Observed,
    Decided,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct StopPayload {
    pub(crate) stage: StopStage,
    pub(crate) decision: Option<String>,
    pub(crate) attempt: Option<u64>,
    pub(crate) missing_artifacts: Vec<String>,
    pub(crate) missing_effects: Vec<String>,
    pub(crate) reason: Option<PublicContentRef>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", content = "value", rename_all = "snake_case")]
pub(crate) enum PublicEventPayload {
    Run(RunLifecyclePayload),
    Revision(RevisionLifecyclePayload),
    Fact(FactPayload),
    Artifact(ArtifactPayload),
    Route(RoutePayload),
    Activation(ActivationLifecyclePayload),
    Session(SessionLifecyclePayload),
    Hook(HookPayload),
    Compaction(CompactionPayload),
    WorkProfile(WorkProfilePayload),
    Qos(QosPayload),
    Usage(UsagePayload),
    MachineInput(MachineInputPayload),
    Stop(StopPayload),
}

pub(crate) struct PublicRefMapper<'a> {
    map: &'a dyn Fn(&str, &str) -> String,
}

impl<'a> PublicRefMapper<'a> {
    pub(crate) fn new(map: &'a dyn Fn(&str, &str) -> String) -> Self {
        Self { map }
    }

    fn reference(&self, namespace: &str, value: &str) -> String {
        (self.map)(namespace, value)
    }
}

impl PublicEventPayload {
    pub(crate) fn materialize(&self, refs: &PublicRefMapper<'_>) -> Value {
        match self {
            Self::Run(payload) => json!({
                "transition": payload.transition,
                "mode": payload.mode,
                "status": payload.status,
                "reason": payload.reason.as_ref().map(PublicContentRef::to_value),
                "activation_limit": payload.activation_limit,
                "stop_attempt_limit": payload.stop_attempt_limit,
            }),
            Self::Revision(payload) => json!({
                "state": payload.state,
                "flow_ref": refs.reference("flow", &payload.flow_id),
                "content": payload.content.to_value(),
                "review_status": payload.review_status,
            }),
            Self::Fact(payload) => json!({
                "fact_kind": payload.fact_kind,
                "key_ref": refs.reference("fact_key", &payload.key),
                "version": payload.version,
                "content": payload.content.to_value(),
            }),
            Self::Artifact(payload) => json!({
                "artifact_ref": refs.reference("artifact", &payload.artifact_id),
                "artifact_key_ref": refs.reference("artifact_key", &payload.artifact_key),
                "content": payload.content.to_value(),
            }),
            Self::Route(payload) => json!({
                "flow_ref": refs.reference("flow", &payload.flow_id),
                "route_ref": refs.reference("route", &payload.route_id),
                "route_index": payload.route_index,
                "predicate_ref": refs.reference("predicate", &payload.predicate),
                "for_each_ref": payload.for_each.as_ref().map(|value| refs.reference("fact", value)),
                "source_artifact_ref": payload.source_artifact_id.as_ref().map(|value| refs.reference("artifact", value)),
                "trigger_ref": refs.reference("fact", &payload.trigger_fact),
                "trigger_version": payload.trigger_version,
                "planned_activation_refs": payload.planned_activation_ids.iter().map(|value| refs.reference("activation", value)).collect::<Vec<_>>(),
                "applied_activation_refs": payload.applied_activation_ids.iter().map(|value| refs.reference("activation", value)).collect::<Vec<_>>(),
            }),
            Self::Activation(payload) => json!({
                "state": payload.state,
                "node_ref": payload.node_id.as_ref().map(|value| refs.reference("node", value)),
                "allocation_generation": payload.allocation_generation,
                "termination": payload.termination.as_ref().map(PublicContentRef::to_value),
            }),
            Self::Session(payload) => json!({
                "state": payload.state,
                "platform": payload.platform,
                "exit_status": payload.exit_status,
            }),
            Self::Hook(payload) => json!({
                "hook_kind": payload.hook_kind,
                "detail": payload.detail.to_value(),
                "status": payload.status,
                "allocation_generation": payload.allocation_generation,
                "context_generation": payload.context_generation,
            }),
            Self::Compaction(payload) => json!({
                "state": payload.state,
                "context_generation": payload.context_generation,
            }),
            Self::WorkProfile(payload) => json!({
                "intent": payload.intent,
                "workspace_access": payload.workspace_access,
                "tool_execution": payload.tool_execution,
                "network_access": payload.network_access,
            }),
            Self::Qos(payload) => json!({
                "state": payload.state,
                "urgency": payload.urgency,
                "completion_target": payload.completion_target.as_ref().map(PublicContentRef::to_value),
            }),
            Self::Usage(payload) => json!({
                "metric": payload.metric,
                "value": payload.value,
            }),
            Self::MachineInput(payload) => json!({
                "status": payload.status,
                "content": payload.content.to_value(),
                "submit_key_count": payload.submit_key_count,
            }),
            Self::Stop(payload) => json!({
                "stage": payload.stage,
                "decision": payload.decision,
                "attempt": payload.attempt,
                "missing_artifact_refs": payload.missing_artifacts.iter().map(|value| refs.reference("artifact_key", value)).collect::<Vec<_>>(),
                "missing_effect_refs": payload.missing_effects.iter().map(|value| refs.reference("effect_key", value)).collect::<Vec<_>>(),
                "reason": payload.reason.as_ref().map(PublicContentRef::to_value),
            }),
        }
    }

    pub(crate) fn matches_kind(&self, kind: PublicEventKind) -> bool {
        matches!(
            (kind, self),
            (
                PublicEventKind::RunStarted,
                Self::Run(RunLifecyclePayload {
                    transition: RunTransition::Started,
                    ..
                })
            ) | (
                PublicEventKind::RunStatus,
                Self::Run(RunLifecyclePayload {
                    transition: RunTransition::Status,
                    ..
                })
            ) | (
                PublicEventKind::RunCompleted,
                Self::Run(RunLifecyclePayload {
                    transition: RunTransition::Completed,
                    ..
                })
            ) | (
                PublicEventKind::FlowRevisionPrepared,
                Self::Revision(RevisionLifecyclePayload {
                    state: RevisionState::Prepared,
                    ..
                })
            ) | (
                PublicEventKind::FlowRevisionApplied,
                Self::Revision(RevisionLifecyclePayload {
                    state: RevisionState::Applied,
                    ..
                })
            ) | (
                PublicEventKind::FlowRevisionRejected,
                Self::Revision(RevisionLifecyclePayload {
                    state: RevisionState::Rejected,
                    ..
                })
            ) | (PublicEventKind::FactRecorded, Self::Fact(_))
                | (PublicEventKind::ArtifactRecorded, Self::Artifact(_))
                | (PublicEventKind::RouteDecided, Self::Route(_))
                | (PublicEventKind::ActivationCreated, Self::Activation(_))
                | (PublicEventKind::ActivationStatus, Self::Activation(_))
                | (PublicEventKind::ActivationCompleted, Self::Activation(_))
                | (
                    PublicEventKind::AgentSessionStarted,
                    Self::Session(SessionLifecyclePayload {
                        state: SessionState::Started,
                        ..
                    })
                )
                | (
                    PublicEventKind::AgentSessionBound,
                    Self::Session(SessionLifecyclePayload {
                        state: SessionState::Bound,
                        ..
                    })
                )
                | (
                    PublicEventKind::AgentSessionEnded,
                    Self::Session(SessionLifecyclePayload {
                        state: SessionState::Ended,
                        ..
                    })
                )
                | (PublicEventKind::HookObserved, Self::Hook(_))
                | (
                    PublicEventKind::ContextCompactionStarted,
                    Self::Compaction(CompactionPayload {
                        state: CompactionState::Started,
                        ..
                    })
                )
                | (
                    PublicEventKind::ContextCompactionFinished,
                    Self::Compaction(CompactionPayload {
                        state: CompactionState::Finished,
                        ..
                    })
                )
                | (PublicEventKind::WorkProfileObserved, Self::WorkProfile(_))
                | (
                    PublicEventKind::QosObserved,
                    Self::Qos(QosPayload {
                        state: QosState::Observed,
                        ..
                    })
                )
                | (
                    PublicEventKind::QosApplied,
                    Self::Qos(QosPayload {
                        state: QosState::Applied,
                        ..
                    })
                )
                | (PublicEventKind::UsageObserved, Self::Usage(_))
                | (
                    PublicEventKind::MachineInputDelivered,
                    Self::MachineInput(_)
                )
                | (
                    PublicEventKind::StopObserved,
                    Self::Stop(StopPayload {
                        stage: StopStage::Observed,
                        ..
                    })
                )
                | (
                    PublicEventKind::StopDecided,
                    Self::Stop(StopPayload {
                        stage: StopStage::Decided,
                        ..
                    })
                )
        )
    }
}

pub(crate) fn validate_known_wire_payload(
    kind: PublicEventKind,
    data: &Value,
) -> Result<(), String> {
    let object = data
        .as_object()
        .ok_or_else(|| "known payload must be an object".to_string())?;
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .filter(|value| {
            matches!(
                *value,
                "driver" | "hook" | "machine_input" | "run_assets" | "runtime"
            )
        })
        .ok_or_else(|| "known payload source is missing or invalid".to_string())?;
    let _ = source;
    require_ref(object.get("source_ref"), "source_ref")?;
    let payload = object
        .get("payload")
        .ok_or_else(|| "known payload is missing payload".to_string())?;
    validate_payload_shape(kind, payload)
}

fn validate_payload_shape(kind: PublicEventKind, payload: &Value) -> Result<(), String> {
    let object = payload
        .as_object()
        .ok_or_else(|| "payload must be an object".to_string())?;
    match kind {
        PublicEventKind::RunStarted
        | PublicEventKind::RunStatus
        | PublicEventKind::RunCompleted => require_string(object.get("transition"), "transition"),
        PublicEventKind::FlowRevisionPrepared
        | PublicEventKind::FlowRevisionApplied
        | PublicEventKind::FlowRevisionRejected => {
            require_string(object.get("state"), "state")?;
            require_ref(object.get("flow_ref"), "flow_ref")?;
            validate_content(object.get("content"), true)
        }
        PublicEventKind::FactRecorded => {
            require_string(object.get("fact_kind"), "fact_kind")?;
            require_ref(object.get("key_ref"), "key_ref")?;
            validate_content(object.get("content"), true)
        }
        PublicEventKind::ArtifactRecorded => {
            require_ref(object.get("artifact_ref"), "artifact_ref")?;
            require_ref(object.get("artifact_key_ref"), "artifact_key_ref")?;
            validate_content(object.get("content"), true)
        }
        PublicEventKind::RouteDecided => {
            require_ref(object.get("flow_ref"), "flow_ref")?;
            require_ref(object.get("route_ref"), "route_ref")?;
            require_u64(object.get("route_index"), "route_index")?;
            require_ref(object.get("trigger_ref"), "trigger_ref")?;
            require_u64(object.get("trigger_version"), "trigger_version")
        }
        PublicEventKind::ActivationCreated
        | PublicEventKind::ActivationStatus
        | PublicEventKind::ActivationCompleted => require_string(object.get("state"), "state"),
        PublicEventKind::AgentSessionStarted
        | PublicEventKind::AgentSessionBound
        | PublicEventKind::AgentSessionEnded => {
            require_string(object.get("state"), "state")?;
            require_string(object.get("platform"), "platform")
        }
        PublicEventKind::HookObserved => {
            require_string(object.get("hook_kind"), "hook_kind")?;
            validate_content(object.get("detail"), false)
        }
        PublicEventKind::ContextCompactionStarted | PublicEventKind::ContextCompactionFinished => {
            require_string(object.get("state"), "state")?;
            require_u64(object.get("context_generation"), "context_generation")
        }
        PublicEventKind::WorkProfileObserved => {
            for field in [
                "intent",
                "workspace_access",
                "tool_execution",
                "network_access",
            ] {
                require_string(object.get(field), field)?;
            }
            Ok(())
        }
        PublicEventKind::QosObserved | PublicEventKind::QosApplied => {
            require_string(object.get("state"), "state")?;
            require_string(object.get("urgency"), "urgency")
        }
        PublicEventKind::UsageObserved => {
            require_string(object.get("metric"), "metric")?;
            require_u64(object.get("value"), "value")
        }
        PublicEventKind::MachineInputDelivered => {
            require_string(object.get("status"), "status")?;
            validate_content(object.get("content"), false)
        }
        PublicEventKind::StopObserved | PublicEventKind::StopDecided => {
            require_string(object.get("stage"), "stage")
        }
    }
}

fn validate_content(value: Option<&Value>, path_required: bool) -> Result<(), String> {
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| "content descriptor is required".to_string())?;
    require_hash(object.get("sha256"), "sha256")?;
    require_hash(object.get("ref"), "ref")?;
    require_u64(object.get("length"), "length")?;
    if path_required {
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty() && !path.starts_with('/') && !path.contains(".."))
            .ok_or_else(|| "content path is required and must be relative".to_string())?;
        let _ = path;
    }
    Ok(())
}

fn require_string(value: Option<&Value>, field: &str) -> Result<(), String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|_| ())
        .ok_or_else(|| format!("{field} is required"))
}

fn require_ref(value: Option<&Value>, field: &str) -> Result<(), String> {
    let value = value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} is required"))?;
    if value.starts_with('/') || value.contains("../") {
        return Err(format!("{field} must be a public reference"));
    }
    Ok(())
}

fn require_hash(value: Option<&Value>, field: &str) -> Result<(), String> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{field} is required"))?;
    if value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        Ok(())
    } else {
        Err(format!("{field} must be a sha256 reference"))
    }
}

fn require_u64(value: Option<&Value>, field: &str) -> Result<(), String> {
    value
        .and_then(Value::as_u64)
        .map(|_| ())
        .ok_or_else(|| format!("{field} is required"))
}

pub(crate) fn wire_data(
    source: PublicSource,
    source_ref: String,
    payload: &PublicEventPayload,
    refs: &PublicRefMapper<'_>,
) -> Value {
    json!({
        "source": source.as_str(),
        "source_ref": source_ref,
        "payload": payload.materialize(refs),
    })
}
