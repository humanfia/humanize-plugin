use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::flow;
use crate::input_ledger::{MachineInputRecord, MachineInputStatus};
use crate::runtime;

use super::journal::{self, JournalReadMode, PublicJournalEvent, PublicJournalInput};
use super::{
    AGENT_READY_FAILURE_HOOK, AGENT_READY_HOOK, RunAssetActivation, RunAssetError,
    RunAssetFlowRevision, RunAssetManifest, RunAssetPreservationError, TMUX_GUARD_BLOCKED_HOOK,
    append_private_line, atomic_write_private, create_dir_all, ensure_private_dir,
    read_regular_private,
};
use super::{
    ActivationLifecyclePayload, ActivationState, ArtifactPayload, CompactionPayload,
    CompactionState, FactKind, FactPayload, HookKind, HookPayload, MachineInputPayload,
    NativePlatform, PublicContentRef, PublicEventKind, PublicEventPayload, PublicSource,
    QosPayload, QosState, RevisionLifecyclePayload, RevisionState, RoutePayload,
    RunLifecyclePayload, RunTransition, SessionLifecyclePayload, SessionState, StopPayload,
    StopStage,
};

const RECORD_ROOT: &str = "records";
const RECORD_WRITER_LOCK_RELATIVE_PATH: &str = "records/.writer.lock";

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetRecordIndex {
    pub root_relative_path: String,
    pub files: BTreeMap<String, RunAssetRecordFile>,
    pub sessions: BTreeMap<String, RunAssetSessionIndex>,
}

impl Default for RunAssetRecordIndex {
    fn default() -> Self {
        Self {
            root_relative_path: RECORD_ROOT.to_string(),
            files: BTreeMap::new(),
            sessions: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetRecordFile {
    pub relative_path: String,
    pub latest_sequence: u64,
    pub record_count: u64,
    pub latest_wall_time_ms: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetSessionIndex {
    pub session_id: String,
    pub context_generation: u64,
    pub relations: Vec<RunAssetSessionRelation>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetSessionRelation {
    pub relation: String,
    pub run_id: String,
    pub activation_id: Option<String>,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SessionRelation {
    Orchestrates,
    Executes,
    Ended,
}

impl SessionRelation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Orchestrates => "orchestrates",
            Self::Executes => "executes",
            Self::Ended => "ended",
        }
    }
}

pub(super) struct SessionFactInput<'a> {
    pub session_id: &'a str,
    pub relation: SessionRelation,
    pub activation_id: Option<&'a str>,
    pub platform: &'a str,
    pub exit_status: Option<i32>,
    pub now_ms: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ActivationProbeState {
    Planned,
    Ready,
    Suspended,
    Closed,
}

impl ActivationProbeState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Ready => "ready",
            Self::Suspended => "suspended",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HookFactKind {
    CompactionPending,
    CompactionFinished,
    Other,
}

impl HookFactKind {
    pub fn parse(value: &str) -> Self {
        match value {
            "compaction_pending" => Self::CompactionPending,
            "compaction_finished" => Self::CompactionFinished,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HookFactInput {
    pub session_id: String,
    pub activation_id: Option<String>,
    pub hook: String,
    pub source_native_id: String,
    pub detail: HookFactDetail,
    pub causal_id: Option<String>,
    pub correlation_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum HookFactDetail {
    AgentReady {
        status: String,
        allocation_generation: u64,
        ready_signal_hash: Option<String>,
    },
    AgentReadyFailure {
        status: String,
        allocation_generation: u64,
        elapsed_ms: u64,
    },
    TmuxGuardBlocked {
        decision: String,
        operation: String,
        option_flags: Vec<String>,
        target_hash: String,
        payload_hash: String,
        payload_length: u64,
    },
    Compaction,
    Other {
        payload_hash: String,
        payload_length: u64,
    },
}

impl HookFactDetail {
    pub fn from_observation(hook: &str, payload: &Value) -> Result<Self, RunAssetError> {
        match hook {
            AGENT_READY_HOOK => Ok(Self::AgentReady {
                status: required_payload_string(payload, "status")?.to_string(),
                allocation_generation: required_payload_u64(payload, "allocation_generation")?,
                ready_signal_hash: payload
                    .get("ready_signal_hash")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }),
            AGENT_READY_FAILURE_HOOK => Ok(Self::AgentReadyFailure {
                status: required_payload_string(payload, "status")?.to_string(),
                allocation_generation: required_payload_u64(payload, "allocation_generation")?,
                elapsed_ms: required_payload_u64(payload, "elapsed_ms")?,
            }),
            TMUX_GUARD_BLOCKED_HOOK => Ok(Self::TmuxGuardBlocked {
                decision: required_payload_string(payload, "decision")?.to_string(),
                operation: required_payload_string(payload, "operation")?.to_string(),
                option_flags: payload
                    .get("option_flags")
                    .and_then(Value::as_array)
                    .ok_or_else(|| RunAssetError::new("hook option_flags must be an array"))?
                    .iter()
                    .map(|value| {
                        value.as_str().map(str::to_string).ok_or_else(|| {
                            RunAssetError::new("hook option_flags must contain strings")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                target_hash: required_payload_string(payload, "target_hash")?.to_string(),
                payload_hash: required_payload_string(payload, "payload_hash")?.to_string(),
                payload_length: required_payload_u64(payload, "payload_length")?,
            }),
            "compaction_pending" | "compaction_finished" => Ok(Self::Compaction),
            _ => {
                let bytes = serde_json::to_vec(payload).map_err(|err| {
                    RunAssetError::new(format!("serialize unknown hook payload failed: {err}"))
                })?;
                Ok(Self::Other {
                    payload_hash: format!("sha256:{:x}", Sha256::digest(&bytes)),
                    payload_length: bytes.len() as u64,
                })
            }
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TopologyDecisionInput {
    pub source: TopologyDecisionSource,
    pub source_native_id: String,
    pub flow_lock_id: String,
    pub route_index: u64,
    pub route_id: String,
    pub predicate: String,
    pub for_each: Option<String>,
    pub source_artifact_id: Option<String>,
    pub trigger_fact_ref: String,
    pub trigger_fact_version: u64,
    pub planned_activation_ids: Vec<String>,
    pub applied_activation_ids: Vec<String>,
    pub causal_id: Option<String>,
    pub correlation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TopologyDecisionSource {
    Driver,
    Runtime,
}

#[derive(Debug, Clone)]
struct RecordInput {
    kind: PublicEventKind,
    source: PublicSource,
    source_native_id: String,
    run_id: Option<String>,
    activation_id: Option<String>,
    session_id: Option<String>,
    wall_time_ms: u64,
    causal_id: Option<String>,
    correlation_id: Option<String>,
    payload: PublicEventPayload,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PublicRecordBatch {
    records: Vec<PublicRecordBatchEntry>,
}

impl PublicRecordBatch {
    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn source_native_ids(&self) -> impl Iterator<Item = &str> {
        self.records
            .iter()
            .map(|record| record.input.source_native_id.as_str())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct PublicRecordBatchEntry {
    input: PublicJournalInput,
    occurred_at_ms: u64,
    files: Vec<PendingPublicFile>,
}

pub(super) fn record_manifest_started(
    _manifest: &mut RunAssetManifest,
    _now_ms: u64,
) -> Result<(), RunAssetError> {
    Ok(())
}

pub(super) fn record_flow_revision(
    batch: &mut PublicRecordBatch,
    manifest: &mut RunAssetManifest,
    revision: &RunAssetFlowRevision,
    state: &str,
    now_ms: u64,
    export_bytes: Option<&[u8]>,
) -> Result<(), RunAssetError> {
    let (kind, revision_state) = match state {
        "prepared" => (
            PublicEventKind::FlowRevisionPrepared,
            RevisionState::Prepared,
        ),
        "applied" => (PublicEventKind::FlowRevisionApplied, RevisionState::Applied),
        "failed" | "rejected" => (
            PublicEventKind::FlowRevisionRejected,
            RevisionState::Rejected,
        ),
        _ => {
            return Err(RunAssetError::new(format!(
                "unsupported flow revision state: {state}"
            )));
        }
    };
    let (content, files) = match export_bytes {
        Some(bytes) => {
            let file = PendingPublicFile {
                relative_path: revision.relative_path.clone(),
                bytes: bytes.to_vec(),
            };
            (
                PublicContentRef::file(
                    format!("sha256:{:x}", Sha256::digest(bytes)),
                    bytes.len() as u64,
                    revision.relative_path.clone(),
                ),
                vec![file],
            )
        }
        None => (
            content_ref_for_existing_file(manifest, &revision.export_path)?,
            Vec::new(),
        ),
    };
    append_record_with_files(
        batch,
        manifest,
        RecordInput {
            kind,
            source: PublicSource::RunAssets,
            source_native_id: format!("flow_revision:{}:{state}", revision.revision_id),
            run_id: Some(manifest.run_id.clone()),
            activation_id: None,
            session_id: None,
            wall_time_ms: now_ms,
            causal_id: None,
            correlation_id: None,
            payload: PublicEventPayload::Revision(RevisionLifecyclePayload {
                state: revision_state,
                flow_id: revision.flow_lock_id.clone(),
                content,
                review_status: Some(revision.review_status.clone()),
            }),
        },
        &files,
    )
}

pub(super) fn record_activation_probe(
    batch: &mut PublicRecordBatch,
    manifest: &mut RunAssetManifest,
    activation_id: &str,
    node_id: &str,
    probe_state: ActivationProbeState,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    let (kind, state) = match probe_state {
        ActivationProbeState::Planned => {
            (PublicEventKind::ActivationCreated, ActivationState::Planned)
        }
        ActivationProbeState::Ready => (PublicEventKind::ActivationStatus, ActivationState::Ready),
        ActivationProbeState::Suspended => (
            PublicEventKind::ActivationStatus,
            ActivationState::Suspended,
        ),
        ActivationProbeState::Closed => (
            PublicEventKind::ActivationCompleted,
            ActivationState::Closed,
        ),
    };
    append_record(
        batch,
        manifest,
        RecordInput {
            kind,
            source: PublicSource::RunAssets,
            source_native_id: format!("activation_probe:{activation_id}:{}", probe_state.as_str()),
            run_id: Some(manifest.run_id.clone()),
            activation_id: Some(activation_id.to_string()),
            session_id: None,
            wall_time_ms: now_ms,
            causal_id: None,
            correlation_id: None,
            payload: PublicEventPayload::Activation(ActivationLifecyclePayload {
                state,
                node_id: Some(node_id.to_string()),
                allocation_generation: None,
                termination: None,
            }),
        },
    )
}

pub(super) fn record_tmux_activation(
    batch: &mut PublicRecordBatch,
    manifest: &mut RunAssetManifest,
    activation: &RunAssetActivation,
    state: &str,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    let (kind, public_state) = match state {
        "capture_started" => (PublicEventKind::ActivationStatus, ActivationState::Starting),
        "capture_acknowledged" | "capturing" => {
            (PublicEventKind::ActivationStatus, ActivationState::Running)
        }
        "capture_completed" => (
            PublicEventKind::ActivationCompleted,
            ActivationState::Completed,
        ),
        "capture_failed" => (
            PublicEventKind::ActivationCompleted,
            ActivationState::Failed,
        ),
        "resource_cleanup_complete" => (
            PublicEventKind::ActivationCompleted,
            ActivationState::Closed,
        ),
        _ => (PublicEventKind::ActivationStatus, ActivationState::Running),
    };
    append_record(
        batch,
        manifest,
        RecordInput {
            kind,
            source: PublicSource::RunAssets,
            source_native_id: format!(
                "activation:{}:allocation:{}:{state}",
                activation.activation_id, activation.allocation_generation
            ),
            run_id: Some(manifest.run_id.clone()),
            activation_id: Some(activation.activation_id.clone()),
            session_id: None,
            wall_time_ms: now_ms,
            causal_id: None,
            correlation_id: None,
            payload: PublicEventPayload::Activation(ActivationLifecyclePayload {
                state: public_state,
                node_id: Some(activation.node_id.clone()),
                allocation_generation: Some(activation.allocation_generation),
                termination: activation
                    .termination_reason
                    .as_deref()
                    .map(hashed_text_ref),
            }),
        },
    )
}

pub(super) fn recorded_tmux_activation_time(
    manifest: &RunAssetManifest,
    activation_id: &str,
    allocation_generation: u64,
    state: &str,
) -> Result<Option<u64>, RunAssetError> {
    let source_ref =
        format!("activation:{activation_id}:allocation:{allocation_generation}:{state}");
    let event = journal::read_events(manifest, JournalReadMode::RecoverTornTail)?
        .into_iter()
        .find(|event| {
            matches!(
                event.kind.as_str(),
                "activation.status" | "activation.completed"
            ) && event.data.get("source_ref").and_then(Value::as_str) == Some(source_ref.as_str())
        });
    Ok(event.map(|event| event.occurred_at_ms))
}

pub(super) fn record_preservation_failure(
    batch: &mut PublicRecordBatch,
    manifest: &mut RunAssetManifest,
    error: &RunAssetPreservationError,
) -> Result<(), RunAssetError> {
    append_record(
        batch,
        manifest,
        RecordInput {
            kind: PublicEventKind::RunStatus,
            source: PublicSource::RunAssets,
            source_native_id: match error.activation_id.as_deref() {
                Some(activation_id) => {
                    format!("preservation:{activation_id}:{}", error.recorded_at_ms)
                }
                None => format!("preservation:run:{}", error.recorded_at_ms),
            },
            run_id: Some(manifest.run_id.clone()),
            activation_id: error.activation_id.clone(),
            session_id: None,
            wall_time_ms: error.recorded_at_ms,
            causal_id: None,
            correlation_id: None,
            payload: PublicEventPayload::Run(RunLifecyclePayload {
                transition: RunTransition::Status,
                mode: None,
                status: Some("publication_blocked".to_string()),
                reason: Some(hashed_text_ref(&error.error)),
                activation_limit: None,
                stop_attempt_limit: None,
            }),
        },
    )
}

pub(super) fn record_session_relation(
    batch: &mut PublicRecordBatch,
    manifest: &mut RunAssetManifest,
    input: SessionFactInput<'_>,
) -> Result<(), RunAssetError> {
    let SessionFactInput {
        session_id,
        relation,
        activation_id,
        platform,
        exit_status,
        now_ms,
    } = input;
    if session_id.is_empty() {
        return Ok(());
    }
    let (kind, state) = match relation {
        SessionRelation::Orchestrates => {
            (PublicEventKind::AgentSessionStarted, SessionState::Started)
        }
        SessionRelation::Executes => (PublicEventKind::AgentSessionBound, SessionState::Bound),
        SessionRelation::Ended => (PublicEventKind::AgentSessionEnded, SessionState::Ended),
    };
    let platform = match platform {
        "codex" => NativePlatform::Codex,
        "claude" => NativePlatform::Claude,
        _ => NativePlatform::Other,
    };
    append_record(
        batch,
        manifest,
        RecordInput {
            kind,
            source: PublicSource::RunAssets,
            source_native_id: session_source_native_id(
                session_id,
                relation,
                activation_id.unwrap_or(&manifest.run_id),
            ),
            run_id: Some(manifest.run_id.clone()),
            activation_id: activation_id.map(str::to_string),
            session_id: Some(session_id.to_string()),
            wall_time_ms: now_ms,
            causal_id: None,
            correlation_id: None,
            payload: PublicEventPayload::Session(SessionLifecyclePayload {
                state,
                platform,
                exit_status,
            }),
        },
    )
}

pub(super) fn record_hook_fact(
    batch: &mut PublicRecordBatch,
    manifest: &mut RunAssetManifest,
    input: HookFactInput,
    now_ms: u64,
) -> Result<u64, RunAssetError> {
    let kind = HookFactKind::parse(&input.hook);
    let context_generation = journal::hook_context_generation(
        manifest,
        &input.session_id,
        &input.source_native_id,
        kind == HookFactKind::CompactionFinished,
    )?;
    let (hook_kind, detail, status, allocation_generation) = hook_payload(&input.detail)?;
    append_record(
        batch,
        manifest,
        RecordInput {
            kind: PublicEventKind::HookObserved,
            source: PublicSource::Hook,
            source_native_id: input.source_native_id.clone(),
            run_id: Some(manifest.run_id.clone()),
            activation_id: input.activation_id.clone(),
            session_id: Some(input.session_id.clone()),
            wall_time_ms: now_ms,
            causal_id: input.causal_id.clone(),
            correlation_id: input.correlation_id.clone(),
            payload: PublicEventPayload::Hook(HookPayload {
                hook_kind,
                detail,
                status,
                allocation_generation,
                context_generation: Some(context_generation),
            }),
        },
    )?;
    if matches!(
        kind,
        HookFactKind::CompactionPending | HookFactKind::CompactionFinished
    ) {
        let (event_kind, state) = match kind {
            HookFactKind::CompactionPending => (
                PublicEventKind::ContextCompactionStarted,
                CompactionState::Started,
            ),
            HookFactKind::CompactionFinished => (
                PublicEventKind::ContextCompactionFinished,
                CompactionState::Finished,
            ),
            HookFactKind::Other => unreachable!("compaction kind was checked"),
        };
        append_record(
            batch,
            manifest,
            RecordInput {
                kind: event_kind,
                source: PublicSource::Hook,
                source_native_id: format!("{}:compaction", input.source_native_id),
                run_id: Some(manifest.run_id.clone()),
                activation_id: input.activation_id,
                session_id: Some(input.session_id),
                wall_time_ms: now_ms,
                causal_id: input.causal_id,
                correlation_id: input.correlation_id,
                payload: PublicEventPayload::Compaction(CompactionPayload {
                    state,
                    context_generation,
                }),
            },
        )?;
    }
    Ok(context_generation)
}

fn hook_payload(
    detail: &HookFactDetail,
) -> Result<(HookKind, PublicContentRef, Option<String>, Option<u64>), RunAssetError> {
    let result = match detail {
        HookFactDetail::AgentReady {
            status,
            allocation_generation,
            ready_signal_hash,
        } => (
            HookKind::AgentReady,
            hashed_json_ref(&json!({ "ready_signal_hash": ready_signal_hash }))?,
            Some(status.clone()),
            Some(*allocation_generation),
        ),
        HookFactDetail::AgentReadyFailure {
            status,
            allocation_generation,
            elapsed_ms,
        } => (
            HookKind::AgentReadyFailure,
            hashed_json_ref(&json!({ "elapsed_ms": elapsed_ms }))?,
            Some(status.clone()),
            Some(*allocation_generation),
        ),
        HookFactDetail::TmuxGuardBlocked {
            decision,
            operation,
            option_flags,
            target_hash,
            payload_hash,
            payload_length,
        } => (
            HookKind::TmuxGuardBlocked,
            hashed_json_ref(&json!({
                "decision": decision,
                "operation": operation,
                "option_flags": option_flags,
                "target_hash": target_hash,
                "payload_hash": payload_hash,
                "payload_length": payload_length,
            }))?,
            Some(decision.clone()),
            None,
        ),
        HookFactDetail::Compaction => (HookKind::Other, hashed_text_ref("compaction"), None, None),
        HookFactDetail::Other {
            payload_hash,
            payload_length,
        } => (
            HookKind::Other,
            PublicContentRef::hashed(payload_hash.clone(), *payload_length),
            None,
            None,
        ),
    };
    Ok(result)
}

pub(super) fn record_machine_input(
    batch: &mut PublicRecordBatch,
    manifest: &mut RunAssetManifest,
    role: &str,
    record: &MachineInputRecord,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    let status = machine_input_status_name(&record.status);
    append_record(
        batch,
        manifest,
        RecordInput {
            kind: PublicEventKind::MachineInputDelivered,
            source: PublicSource::MachineInput,
            source_native_id: machine_input_source_native_id(&record.transaction_id),
            run_id: Some(record.run_id.clone()),
            activation_id: Some(record.activation_id.clone()),
            session_id: None,
            wall_time_ms: now_ms,
            causal_id: None,
            correlation_id: Some(record.transaction_id.clone()),
            payload: PublicEventPayload::MachineInput(MachineInputPayload {
                status: format!("{role}:{status}"),
                content: PublicContentRef::hashed(
                    record.payload_hash.clone(),
                    record.normalized_text.len() as u64,
                ),
                submit_key_count: record.submit_key_count as u64,
            }),
        },
    )
}

pub(crate) fn session_source_native_id(
    session_id: &str,
    relation: SessionRelation,
    subject_id: &str,
) -> String {
    format!("session:{session_id}:{}:{subject_id}", relation.as_str())
}

pub(crate) fn machine_input_source_native_id(transaction_id: &str) -> String {
    format!("machine_input:{transaction_id}")
}

pub(super) fn record_qos_intent(
    batch: &mut PublicRecordBatch,
    manifest: &mut RunAssetManifest,
    qos: &flow::FlowQosIntent,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    append_record(
        batch,
        manifest,
        RecordInput {
            kind: PublicEventKind::QosObserved,
            source: PublicSource::RunAssets,
            source_native_id: "qos:run_intent".to_string(),
            run_id: Some(manifest.run_id.clone()),
            activation_id: None,
            session_id: None,
            wall_time_ms: now_ms,
            causal_id: None,
            correlation_id: None,
            payload: PublicEventPayload::Qos(QosPayload {
                state: QosState::Observed,
                urgency: qos_urgency_name(qos.urgency).to_string(),
                completion_target: qos.completion_target.as_deref().map(hashed_text_ref),
            }),
        },
    )
}

pub(super) fn record_topology_decision(
    batch: &mut PublicRecordBatch,
    manifest: &mut RunAssetManifest,
    input: TopologyDecisionInput,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    append_record(
        batch,
        manifest,
        RecordInput {
            kind: PublicEventKind::RouteDecided,
            source: match input.source {
                TopologyDecisionSource::Driver => PublicSource::Driver,
                TopologyDecisionSource::Runtime => PublicSource::Runtime,
            },
            source_native_id: input.source_native_id,
            run_id: Some(manifest.run_id.clone()),
            activation_id: None,
            session_id: None,
            wall_time_ms: now_ms,
            causal_id: input.causal_id,
            correlation_id: input.correlation_id,
            payload: PublicEventPayload::Route(RoutePayload {
                flow_id: input.flow_lock_id,
                route_index: input.route_index,
                route_id: input.route_id,
                predicate: input.predicate,
                for_each: input.for_each,
                source_artifact_id: input.source_artifact_id,
                trigger_fact: input.trigger_fact_ref,
                trigger_version: input.trigger_fact_version,
                planned_activation_ids: input.planned_activation_ids,
                applied_activation_ids: input.applied_activation_ids,
            }),
        },
    )
}

pub(super) fn record_runtime_event(
    batch: &mut PublicRecordBatch,
    manifest: &mut RunAssetManifest,
    event: &runtime::Event,
    now_ms: u64,
) -> Result<bool, RunAssetError> {
    let Some((input, files)) = runtime_record_input(manifest, event, now_ms)? else {
        return Ok(false);
    };
    append_record_with_files(batch, manifest, input, &files)?;
    Ok(true)
}

pub(crate) fn prepare_runtime_publication(
    manifest: &RunAssetManifest,
    events: &[runtime::Event],
    routes: &[runtime::RouteDecision],
    occurred_at_ms: u64,
) -> Result<PublicRecordBatch, RunAssetError> {
    let mut records = Vec::new();
    for event in events {
        let Some((input, files)) = runtime_record_input(manifest, event, occurred_at_ms)? else {
            continue;
        };
        records.push(PublicRecordBatchEntry {
            input: journal_input_from_record(manifest, input),
            occurred_at_ms,
            files,
        });
    }
    for decision in routes {
        let input = route_record_input(manifest, decision, occurred_at_ms);
        records.push(PublicRecordBatchEntry {
            input: journal_input_from_record(manifest, input),
            occurred_at_ms,
            files: Vec::new(),
        });
    }
    Ok(PublicRecordBatch { records })
}

pub(crate) fn publish_record_batch(
    manifest: &RunAssetManifest,
    batch: &PublicRecordBatch,
) -> Result<(), RunAssetError> {
    if batch.records.is_empty() {
        return Ok(());
    }
    let _lock = RecordStoreLock::acquire(manifest)?;
    let prepared = journal::prepare_event_batch(
        manifest,
        batch
            .records
            .iter()
            .map(|record| (record.input.clone(), record.occurred_at_ms))
            .collect(),
    )?;
    for record in &batch.records {
        for file in &record.files {
            persist_content_addressed_file(manifest, file)?;
        }
    }
    journal::append_prepared_event_batch(manifest, prepared)?;
    Ok(())
}

pub(crate) fn preflight_record_batch(
    manifest: &RunAssetManifest,
    batch: &PublicRecordBatch,
) -> Result<(), RunAssetError> {
    if batch.records.is_empty() {
        return Ok(());
    }
    let _lock = RecordStoreLock::acquire(manifest)?;
    journal::prepare_event_batch(
        manifest,
        batch
            .records
            .iter()
            .map(|record| (record.input.clone(), record.occurred_at_ms))
            .collect(),
    )?;
    Ok(())
}

fn append_record(
    batch: &mut PublicRecordBatch,
    manifest: &RunAssetManifest,
    input: RecordInput,
) -> Result<(), RunAssetError> {
    append_record_with_files(batch, manifest, input, &[])
}

fn append_record_with_files(
    batch: &mut PublicRecordBatch,
    manifest: &RunAssetManifest,
    input: RecordInput,
    files: &[PendingPublicFile],
) -> Result<(), RunAssetError> {
    let occurred_at_ms = input.wall_time_ms;
    batch.records.push(PublicRecordBatchEntry {
        input: journal_input_from_record(manifest, input),
        occurred_at_ms,
        files: files.to_vec(),
    });
    Ok(())
}

fn journal_input_from_record(
    manifest: &RunAssetManifest,
    input: RecordInput,
) -> PublicJournalInput {
    let RecordInput {
        kind,
        source,
        source_native_id,
        run_id,
        activation_id,
        session_id,
        wall_time_ms: _,
        causal_id,
        correlation_id,
        payload,
    } = input;
    PublicJournalInput {
        kind,
        run_id: run_id.unwrap_or_else(|| manifest.run_id.clone()),
        activation_id,
        session_id,
        revision_id: None,
        caused_by_seq: causal_id.as_deref().and_then(|value| value.parse().ok()),
        correlation_id,
        source,
        source_native_id,
        payload,
    }
}

pub(super) fn append_machine_input_ledger_direct(
    path: &Path,
    record: &MachineInputRecord,
) -> Result<(), RunAssetError> {
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    create_dir_all(&root.join(RECORD_ROOT))?;
    ensure_private_dir(&root.join(RECORD_ROOT))?;
    let _lock = RecordStoreLock::acquire_root(root)?;
    let records = read_machine_input_ledger_records_at(root, path)?;
    if records
        .iter()
        .any(|existing| machine_input_records_match(existing, record))
    {
        return Ok(());
    }
    append_machine_input_ledger_payload(path, record)
}

fn append_machine_input_ledger_payload(
    path: &Path,
    record: &MachineInputRecord,
) -> Result<(), RunAssetError> {
    let line = serde_json::to_vec(record).map_err(|err| {
        RunAssetError::new(format!("serialize machine input ledger failed: {err}"))
    })?;
    let mut payload = line;
    payload.push(b'\n');
    append_private_line(path, &payload).map_err(|err| {
        RunAssetError::new(format!(
            "append machine input ledger {} failed: {err}",
            path.display()
        ))
    })
}

fn machine_input_records_match(existing: &MachineInputRecord, record: &MachineInputRecord) -> bool {
    existing.transaction_id == record.transaction_id && existing.status == record.status
}

pub(super) fn rebuild_record_index(
    manifest: &RunAssetManifest,
) -> Result<RunAssetRecordIndex, RunAssetError> {
    let _lock = RecordStoreLock::acquire(manifest)?;
    rebuild_index_from_journal(manifest, JournalReadMode::RecoverTornTail)
}

pub(super) fn record_index(
    manifest: &RunAssetManifest,
) -> Result<RunAssetRecordIndex, RunAssetError> {
    rebuild_index_from_journal(manifest, JournalReadMode::Strict)
}

fn rebuild_index_from_journal(
    manifest: &RunAssetManifest,
    mode: JournalReadMode,
) -> Result<RunAssetRecordIndex, RunAssetError> {
    let mut index = RunAssetRecordIndex::default();
    for event in journal::read_events(manifest, mode)? {
        index_record_file(
            &mut index,
            "events",
            journal::EVENT_LOG_RELATIVE_PATH,
            event.seq,
            event.occurred_at_ms,
        );
        index_journal_session_fact(&mut index, &event);
    }
    Ok(index)
}

fn quarantine_torn_tail_at(
    root: &Path,
    stream: &str,
    torn_tail: &[u8],
) -> Result<(), RunAssetError> {
    let quarantine_path = root.join(RECORD_ROOT).join("quarantine").join(format!(
        "{stream}-torn-tail-{:016x}-{}.fragment",
        stable_tail_hash(torn_tail),
        torn_tail.len()
    ));
    atomic_write_private(&quarantine_path, torn_tail).map_err(|err| {
        RunAssetError::new(format!(
            "quarantine torn durable record tail {} failed: {err}",
            quarantine_path.display()
        ))
    })
}

fn stable_tail_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn index_record_file(
    index: &mut RunAssetRecordIndex,
    stream: &str,
    relative_path: &str,
    sequence: u64,
    wall_time_ms: u64,
) {
    let file = index
        .files
        .entry(stream.to_string())
        .or_insert_with(|| record_file(relative_path));
    file.relative_path = relative_path.to_string();
    file.latest_sequence = file.latest_sequence.max(sequence);
    file.record_count = file.record_count.saturating_add(1);
    file.latest_wall_time_ms = file.latest_wall_time_ms.max(wall_time_ms);
}

fn record_file(relative_path: &str) -> RunAssetRecordFile {
    RunAssetRecordFile {
        relative_path: relative_path.to_string(),
        latest_sequence: 0,
        record_count: 0,
        latest_wall_time_ms: 0,
    }
}

fn index_journal_session_fact(index: &mut RunAssetRecordIndex, event: &PublicJournalEvent) {
    if let Some(session_ref) = event.session_ref.as_deref() {
        let session = index
            .sessions
            .entry(session_ref.to_string())
            .or_insert_with(|| RunAssetSessionIndex {
                session_id: session_ref.to_string(),
                context_generation: 0,
                relations: Vec::new(),
            });
        if let Some(generation) = event
            .data
            .pointer("/payload/context_generation")
            .and_then(Value::as_u64)
        {
            session.context_generation = session.context_generation.max(generation);
        }
    }
    if !matches!(
        event.kind.as_str(),
        "agent_session.started" | "agent_session.bound" | "agent_session.ended"
    ) {
        return;
    }
    let Some(session_ref) = event.session_ref.as_deref() else {
        return;
    };
    let relation = match event.kind.as_str() {
        "agent_session.started" => "orchestrates",
        "agent_session.bound" => "executes",
        "agent_session.ended" => "ended",
        _ => return,
    };
    index_session_relation_by_name(
        index,
        session_ref,
        relation,
        &event.run_ref,
        event.activation_ref.clone(),
        event.occurred_at_ms,
    );
}

fn machine_input_status_name(status: &MachineInputStatus) -> &'static str {
    match status {
        MachineInputStatus::Started => "started",
        MachineInputStatus::Submitted => "submitted",
        MachineInputStatus::Failed => "failed",
    }
}

fn index_session_relation_by_name(
    index: &mut RunAssetRecordIndex,
    session_id: &str,
    relation: &str,
    run_id: &str,
    activation_id: Option<String>,
    now_ms: u64,
) {
    let indexed = RunAssetSessionRelation {
        relation: relation.to_string(),
        run_id: run_id.to_string(),
        activation_id,
        recorded_at_ms: now_ms,
    };
    let session = index
        .sessions
        .entry(session_id.to_string())
        .or_insert_with(|| RunAssetSessionIndex {
            session_id: session_id.to_string(),
            context_generation: 0,
            relations: Vec::new(),
        });
    if !session.relations.iter().any(|existing| {
        existing.relation == indexed.relation
            && existing.run_id == indexed.run_id
            && existing.activation_id == indexed.activation_id
    }) {
        session.relations.push(indexed);
    }
}

fn read_machine_input_ledger_records_at(
    root: &Path,
    path: &Path,
) -> Result<Vec<MachineInputRecord>, RunAssetError> {
    let Some(bytes) = read_regular_private(path).map_err(|err| {
        RunAssetError::new(format!(
            "read machine input ledger {} failed: {err}",
            path.display()
        ))
    })?
    else {
        return Ok(Vec::new());
    };
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return recover_machine_input_ledger_torn_tail(root, path, &bytes);
    }
    parse_machine_input_ledger_records_bytes(path, &bytes)
}

fn recover_machine_input_ledger_torn_tail(
    root: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<MachineInputRecord>, RunAssetError> {
    let prefix_end = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let (committed, torn_tail) = bytes.split_at(prefix_end);
    let records = parse_machine_input_ledger_records_bytes(path, committed)?;
    quarantine_torn_tail_at(root, "machine-inputs", torn_tail)?;
    atomic_write_private(path, committed).map_err(|err| {
        RunAssetError::new(format!(
            "recover machine input ledger {} failed: {err}",
            path.display()
        ))
    })?;
    Ok(records)
}

fn parse_machine_input_ledger_records_bytes(
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<MachineInputRecord>, RunAssetError> {
    let mut records = Vec::new();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        records.push(serde_json::from_slice(line).map_err(|err| {
            RunAssetError::new(format!(
                "parse machine input ledger {} line {} failed: {err}",
                path.display(),
                index + 1
            ))
        })?);
    }
    Ok(records)
}

struct RecordStoreLock {
    #[allow(dead_code)]
    file: Option<fs::File>,
}

impl RecordStoreLock {
    fn acquire(manifest: &RunAssetManifest) -> Result<Self, RunAssetError> {
        Self::acquire_root(&manifest.root)
    }

    fn acquire_root(root: &Path) -> Result<Self, RunAssetError> {
        let legacy_public_lock = root.join(RECORD_WRITER_LOCK_RELATIVE_PATH);
        if legacy_public_lock.exists() {
            return Err(RunAssetError::new(
                "public record writer lock is no longer authoritative",
            ));
        }
        Ok(Self { file: None })
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct PendingPublicFile {
    relative_path: String,
    bytes: Vec<u8>,
}

fn runtime_record_input(
    manifest: &RunAssetManifest,
    event: &runtime::Event,
    now_ms: u64,
) -> Result<Option<(RecordInput, Vec<PendingPublicFile>)>, RunAssetError> {
    let source_native_id = format!("runtime_event:{}", event.sequence);
    let base = |kind, run_id: &str, activation_id: Option<&str>, payload| RecordInput {
        kind,
        source: PublicSource::Runtime,
        source_native_id: source_native_id.clone(),
        run_id: Some(run_id.to_string()),
        activation_id: activation_id.map(str::to_string),
        session_id: None,
        wall_time_ms: now_ms,
        causal_id: None,
        correlation_id: event.correlation.clone(),
        payload,
    };
    let result = match &event.payload {
        runtime::EventPayload::RunStarted {
            run_id,
            mode,
            activation_limit,
            stop_attempt_limit,
        } => (
            base(
                PublicEventKind::RunStarted,
                run_id,
                None,
                PublicEventPayload::Run(RunLifecyclePayload {
                    transition: RunTransition::Started,
                    mode: Some(run_mode_name(*mode).to_string()),
                    status: Some("running".to_string()),
                    reason: None,
                    activation_limit: Some(*activation_limit),
                    stop_attempt_limit: Some(u64::from(*stop_attempt_limit)),
                }),
            ),
            Vec::new(),
        ),
        runtime::EventPayload::RunActivationLimitChanged {
            run_id,
            activation_limit,
        } => (
            base(
                PublicEventKind::RunStatus,
                run_id,
                None,
                PublicEventPayload::Run(RunLifecyclePayload {
                    transition: RunTransition::Status,
                    mode: None,
                    status: Some("activation_limit_changed".to_string()),
                    reason: None,
                    activation_limit: Some(*activation_limit),
                    stop_attempt_limit: None,
                }),
            ),
            Vec::new(),
        ),
        runtime::EventPayload::RunStatusChanged {
            run_id,
            status,
            reason,
        } => {
            let terminal = matches!(
                status,
                runtime::RunStatus::Completed
                    | runtime::RunStatus::Failed
                    | runtime::RunStatus::Stopped
            );
            (
                base(
                    if terminal {
                        PublicEventKind::RunCompleted
                    } else {
                        PublicEventKind::RunStatus
                    },
                    run_id,
                    None,
                    PublicEventPayload::Run(RunLifecyclePayload {
                        transition: if terminal {
                            RunTransition::Completed
                        } else {
                            RunTransition::Status
                        },
                        mode: None,
                        status: Some(run_status_name(*status).to_string()),
                        reason: reason.as_deref().map(hashed_text_ref),
                        activation_limit: None,
                        stop_attempt_limit: None,
                    }),
                ),
                Vec::new(),
            )
        }
        runtime::EventPayload::NodeActivated {
            run_id,
            activation_id,
            node_id,
            activation_generation,
            ..
        } => (
            base(
                PublicEventKind::ActivationCreated,
                run_id,
                Some(activation_id),
                PublicEventPayload::Activation(ActivationLifecyclePayload {
                    state: ActivationState::Planned,
                    node_id: Some(node_id.clone()),
                    allocation_generation: Some(*activation_generation),
                    termination: None,
                }),
            ),
            Vec::new(),
        ),
        runtime::EventPayload::ActivationStatusChanged {
            run_id,
            activation_id,
            status,
        } => {
            let state = public_activation_state(*status);
            let terminal = matches!(
                state,
                ActivationState::Completed | ActivationState::Failed | ActivationState::Cancelled
            );
            (
                base(
                    if terminal {
                        PublicEventKind::ActivationCompleted
                    } else {
                        PublicEventKind::ActivationStatus
                    },
                    run_id,
                    Some(activation_id),
                    PublicEventPayload::Activation(ActivationLifecyclePayload {
                        state,
                        node_id: None,
                        allocation_generation: None,
                        termination: None,
                    }),
                ),
                Vec::new(),
            )
        }
        runtime::EventPayload::ParticipantExited {
            run_id,
            activation_id,
            allocation_generation,
            exit_status,
        } => {
            let bytes = serde_json::to_vec(&json!({
                "allocation_generation": allocation_generation,
                "exit_status": exit_status,
            }))
            .map_err(|err| {
                RunAssetError::new(format!("serialize participant exit fact failed: {err}"))
            })?;
            let (content, file) = pending_content_file("facts", "json", bytes);
            (
                base(
                    PublicEventKind::FactRecorded,
                    run_id,
                    Some(activation_id),
                    PublicEventPayload::Fact(FactPayload {
                        fact_kind: FactKind::Explicit,
                        key: "participant_exit".to_string(),
                        version: Some(event.sequence),
                        content,
                    }),
                ),
                vec![file],
            )
        }
        runtime::EventPayload::ArtifactDelivered {
            run_id,
            activation_id,
            artifact_id,
            artifact_key,
            payload,
            ..
        } => {
            let (content, file) =
                pending_content_file("artifacts", "bin", payload.as_bytes().to_vec());
            (
                base(
                    PublicEventKind::ArtifactRecorded,
                    run_id,
                    Some(activation_id),
                    PublicEventPayload::Artifact(ArtifactPayload {
                        artifact_id: artifact_id.clone(),
                        artifact_key: artifact_key.clone(),
                        content,
                    }),
                ),
                vec![file],
            )
        }
        runtime::EventPayload::BoardPatched {
            run_id,
            activation_id,
            key,
            value,
            version,
        } => {
            let (content, file) = pending_content_file("facts", "bin", value.as_bytes().to_vec());
            (
                base(
                    PublicEventKind::FactRecorded,
                    run_id,
                    Some(activation_id),
                    PublicEventPayload::Fact(FactPayload {
                        fact_kind: FactKind::Board,
                        key: key.clone(),
                        version: Some(*version),
                        content,
                    }),
                ),
                vec![file],
            )
        }
        runtime::EventPayload::EffectRecorded {
            run_id,
            activation_id,
            effect_key,
            payload,
        } => {
            let (content, file) = pending_content_file("facts", "bin", payload.as_bytes().to_vec());
            (
                base(
                    PublicEventKind::FactRecorded,
                    run_id,
                    Some(activation_id),
                    PublicEventPayload::Fact(FactPayload {
                        fact_kind: FactKind::Effect,
                        key: effect_key.clone(),
                        version: Some(event.sequence),
                        content,
                    }),
                ),
                vec![file],
            )
        }
        runtime::EventPayload::StopObserved {
            run_id,
            activation_id,
            observation,
        } => (
            base(
                PublicEventKind::StopObserved,
                run_id,
                Some(activation_id),
                PublicEventPayload::Stop(StopPayload {
                    stage: StopStage::Observed,
                    decision: None,
                    attempt: None,
                    missing_artifacts: Vec::new(),
                    missing_effects: Vec::new(),
                    reason: Some(hashed_text_ref(&observation.reason)),
                }),
            ),
            Vec::new(),
        ),
        runtime::EventPayload::StopDecision {
            run_id,
            activation_id,
            decision,
        } => (
            base(
                PublicEventKind::StopDecided,
                run_id,
                Some(activation_id),
                PublicEventPayload::Stop(StopPayload {
                    stage: StopStage::Decided,
                    decision: Some(stop_decision_kind_name(decision.kind).to_string()),
                    attempt: Some(u64::from(decision.attempt)),
                    missing_artifacts: decision.missing_artifacts.clone(),
                    missing_effects: decision.missing_effects.clone(),
                    reason: decision.reason.as_deref().map(hashed_text_ref),
                }),
            ),
            Vec::new(),
        ),
        runtime::EventPayload::FlowApplied { .. } | runtime::EventPayload::FlowUpdate { .. } => {
            return Ok(None);
        }
    };
    let _ = manifest;
    Ok(Some(result))
}

fn route_record_input(
    manifest: &RunAssetManifest,
    decision: &runtime::RouteDecision,
    now_ms: u64,
) -> RecordInput {
    RecordInput {
        kind: PublicEventKind::RouteDecided,
        source: PublicSource::Runtime,
        source_native_id: format!(
            "route:{}:{}:{}:{}",
            decision.flow_lock_id,
            decision.route_index,
            decision.trigger.fact_ref,
            decision.trigger.fact_version
        ),
        run_id: Some(manifest.run_id.clone()),
        activation_id: None,
        session_id: None,
        wall_time_ms: now_ms,
        causal_id: decision
            .source_artifact
            .as_ref()
            .and_then(|artifact| artifact.artifact_id.clone()),
        correlation_id: Some(decision.route_id.clone()),
        payload: PublicEventPayload::Route(RoutePayload {
            flow_id: decision.flow_lock_id.clone(),
            route_index: decision.route_index as u64,
            route_id: decision.route_id.clone(),
            predicate: decision.predicate.clone(),
            for_each: decision.for_each.clone(),
            source_artifact_id: decision
                .source_artifact
                .as_ref()
                .and_then(|artifact| artifact.artifact_id.clone()),
            trigger_fact: decision.trigger.fact_ref.clone(),
            trigger_version: decision.trigger.fact_version,
            planned_activation_ids: decision.planned_activation_ids.clone(),
            applied_activation_ids: decision.applied_activation_ids.clone(),
        }),
    }
}

fn public_activation_state(status: runtime::ActivationStatus) -> ActivationState {
    match status {
        runtime::ActivationStatus::Pending => ActivationState::Planned,
        runtime::ActivationStatus::Starting => ActivationState::Starting,
        runtime::ActivationStatus::Running => ActivationState::Running,
        runtime::ActivationStatus::WaitingForStop => ActivationState::WaitingForStop,
        runtime::ActivationStatus::ValidatingStop => ActivationState::ValidatingStop,
        runtime::ActivationStatus::Blocked => ActivationState::Blocked,
        runtime::ActivationStatus::Completed => ActivationState::Completed,
        runtime::ActivationStatus::Failed => ActivationState::Failed,
        runtime::ActivationStatus::Cancelled => ActivationState::Cancelled,
    }
}

fn pending_content_file(
    category: &str,
    extension: &str,
    bytes: Vec<u8>,
) -> (PublicContentRef, PendingPublicFile) {
    let hash = format!("sha256:{:x}", Sha256::digest(&bytes));
    let hex = hash.trim_start_matches("sha256:");
    let relative_path = format!("content/{category}/{hex}.{extension}");
    (
        PublicContentRef::file(hash, bytes.len() as u64, relative_path.clone()),
        PendingPublicFile {
            relative_path,
            bytes,
        },
    )
}

fn persist_content_addressed_file(
    manifest: &RunAssetManifest,
    file: &PendingPublicFile,
) -> Result<(), RunAssetError> {
    let relative = Path::new(&file.relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(RunAssetError::new(
            "public content path must be a contained relative path",
        ));
    }
    let path = manifest.root.join(relative);
    if let Some(existing) = read_regular_private(&path)? {
        if existing == file.bytes {
            return Ok(());
        }
        return Err(RunAssetError::new(format!(
            "immutable public content file conflicts at {}",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
        ensure_private_dir(parent)?;
    }
    super::write_create_new_private(&path, &file.bytes)
}

fn content_ref_for_existing_file(
    manifest: &RunAssetManifest,
    path: &Path,
) -> Result<PublicContentRef, RunAssetError> {
    let bytes = read_regular_private(path)?.ok_or_else(|| {
        RunAssetError::new(format!("public content file {} is missing", path.display()))
    })?;
    let relative_path = path
        .strip_prefix(&manifest.root)
        .map_err(|_| RunAssetError::new("public content file is outside the run root"))?
        .to_string_lossy()
        .replace('\\', "/");
    Ok(PublicContentRef::file(
        format!("sha256:{:x}", Sha256::digest(&bytes)),
        bytes.len() as u64,
        relative_path,
    ))
}

fn hashed_text_ref(value: &str) -> PublicContentRef {
    PublicContentRef::hashed(
        format!("sha256:{:x}", Sha256::digest(value.as_bytes())),
        value.len() as u64,
    )
}

fn hashed_json_ref(value: &Value) -> Result<PublicContentRef, RunAssetError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|err| RunAssetError::new(format!("serialize public fact detail failed: {err}")))?;
    Ok(PublicContentRef::hashed(
        format!("sha256:{:x}", Sha256::digest(&bytes)),
        bytes.len() as u64,
    ))
}

fn required_payload_string<'a>(payload: &'a Value, field: &str) -> Result<&'a str, RunAssetError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RunAssetError::new(format!("hook payload field {field} is required")))
}

fn required_payload_u64(payload: &Value, field: &str) -> Result<u64, RunAssetError> {
    payload
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| RunAssetError::new(format!("hook payload field {field} is required")))
}

fn run_mode_name(mode: runtime::RunMode) -> &'static str {
    match mode {
        runtime::RunMode::Finite => "finite",
        runtime::RunMode::Continuous => "continuous",
        runtime::RunMode::Manual => "manual",
    }
}

fn run_status_name(status: runtime::RunStatus) -> &'static str {
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

fn stop_decision_kind_name(kind: runtime::StopDecisionKind) -> &'static str {
    match kind {
        runtime::StopDecisionKind::Allow => "allow",
        runtime::StopDecisionKind::Deny => "deny",
        runtime::StopDecisionKind::Block => "block",
        runtime::StopDecisionKind::Yield => "yield",
    }
}

fn qos_urgency_name(urgency: flow::QosUrgency) -> &'static str {
    match urgency {
        flow::QosUrgency::Interactive => "interactive",
        flow::QosUrgency::Standard => "standard",
        flow::QosUrgency::Background => "background",
    }
}
