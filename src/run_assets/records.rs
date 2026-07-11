use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::flow;
use crate::input_ledger::MachineInputRecord;
use crate::runtime;

use super::{
    RunAssetActivation, RunAssetError, RunAssetFlowRevision, RunAssetManifest,
    RunAssetPreservationError, append_private_line, atomic_write_private, create_dir_all,
    ensure_private_dir, read_regular_private,
};

const RECORD_ROOT: &str = "records";
const RECORD_INDEX_RELATIVE_PATH: &str = "records/index.json";
const RECORD_WRITER_LOCK_RELATIVE_PATH: &str = "records/.writer.lock";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum StreamReadMode {
    Strict,
    RecoverTornTail,
}

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
}

impl SessionRelation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Orchestrates => "orchestrates",
            Self::Executes => "executes",
        }
    }
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
    pub payload: Value,
    pub causal_id: Option<String>,
    pub correlation_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TopologyDecisionInput {
    pub source: &'static str,
    pub source_native_id: String,
    pub fact: Value,
    pub causal_id: Option<String>,
    pub correlation_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct DurableRecord {
    record_id: String,
    run_id: Option<String>,
    activation_id: Option<String>,
    session_id: Option<String>,
    source: String,
    source_native_id: String,
    wall_time_ms: u64,
    source_sequence: u64,
    causal_id: Option<String>,
    correlation_id: Option<String>,
    fact: Value,
}

#[derive(Debug, Clone)]
struct RecordInput {
    stream: &'static str,
    source: &'static str,
    source_native_id: String,
    run_id: Option<String>,
    activation_id: Option<String>,
    session_id: Option<String>,
    wall_time_ms: u64,
    source_sequence: Option<u64>,
    causal_id: Option<String>,
    correlation_id: Option<String>,
    fact: Value,
}

pub(super) fn record_manifest_started(
    manifest: &mut RunAssetManifest,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    append_record(
        manifest,
        RecordInput {
            stream: "storage",
            source: "run_assets",
            source_native_id: "run_asset_manifest:started".to_string(),
            run_id: Some(manifest.run_id.clone()),
            activation_id: None,
            session_id: None,
            wall_time_ms: now_ms,
            source_sequence: None,
            causal_id: None,
            correlation_id: None,
            fact: json!({
                "manifest_path": "manifest.json",
                "storage": manifest.storage,
                "sink": manifest.sink,
            }),
        },
    )
}

pub(super) fn record_flow_revision(
    manifest: &mut RunAssetManifest,
    revision: &RunAssetFlowRevision,
    state: &str,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    append_record(
        manifest,
        RecordInput {
            stream: "flow",
            source: "run_assets",
            source_native_id: format!("flow_revision:{}:{state}", revision.revision_id),
            run_id: Some(manifest.run_id.clone()),
            activation_id: None,
            session_id: None,
            wall_time_ms: now_ms,
            source_sequence: None,
            causal_id: None,
            correlation_id: None,
            fact: json!({
                "revision": revision,
            }),
        },
    )
}

pub(super) fn record_activation_probe(
    manifest: &mut RunAssetManifest,
    activation_id: &str,
    node_id: &str,
    state: ActivationProbeState,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    append_record(
        manifest,
        RecordInput {
            stream: "probe",
            source: "run_assets",
            source_native_id: format!("activation_probe:{activation_id}:{}", state.as_str()),
            run_id: Some(manifest.run_id.clone()),
            activation_id: Some(activation_id.to_string()),
            session_id: None,
            wall_time_ms: now_ms,
            source_sequence: None,
            causal_id: None,
            correlation_id: None,
            fact: json!({
                "activation_id": activation_id,
                "node_id": node_id,
                "probe_state": state.as_str(),
            }),
        },
    )
}

pub(super) fn record_tmux_activation(
    manifest: &mut RunAssetManifest,
    activation: &RunAssetActivation,
    state: &str,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    append_record(
        manifest,
        RecordInput {
            stream: "tmux",
            source: "run_assets",
            source_native_id: format!("activation:{}:{state}", activation.activation_id),
            run_id: Some(manifest.run_id.clone()),
            activation_id: Some(activation.activation_id.clone()),
            session_id: non_empty_string(&activation.session_id),
            wall_time_ms: now_ms,
            source_sequence: None,
            causal_id: None,
            correlation_id: None,
            fact: json!({
                "activation": activation,
                "capture_state": state,
            }),
        },
    )
}

pub(super) fn record_preservation_failure(
    manifest: &mut RunAssetManifest,
    error: &RunAssetPreservationError,
) -> Result<(), RunAssetError> {
    append_record(
        manifest,
        RecordInput {
            stream: "preservation",
            source: "run_assets",
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
            source_sequence: None,
            causal_id: None,
            correlation_id: None,
            fact: json!({
                "preservation_error": error,
            }),
        },
    )
}

pub(super) fn record_session_relation(
    manifest: &mut RunAssetManifest,
    session_id: &str,
    relation: SessionRelation,
    activation_id: Option<&str>,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    if session_id.is_empty() {
        return Ok(());
    }
    with_record_index(manifest, |index| {
        index_session_relation(index, manifest, session_id, relation, activation_id, now_ms);
        append_record_with_index(
            manifest,
            RecordInput {
                stream: "session",
                source: "run_assets",
                source_native_id: format!(
                    "session:{session_id}:{}:{}",
                    relation.as_str(),
                    activation_id.unwrap_or(&manifest.run_id)
                ),
                run_id: Some(manifest.run_id.clone()),
                activation_id: activation_id.map(str::to_string),
                session_id: Some(session_id.to_string()),
                wall_time_ms: now_ms,
                source_sequence: None,
                causal_id: None,
                correlation_id: None,
                fact: json!({
                    "relation": relation.as_str(),
                    "run_id": manifest.run_id,
                    "activation_id": activation_id,
                }),
            },
            index,
        )
    })
}

pub(super) fn record_hook_fact(
    manifest: &mut RunAssetManifest,
    input: HookFactInput,
    now_ms: u64,
) -> Result<u64, RunAssetError> {
    with_record_index(manifest, |index| {
        if let Some(existing) = read_stream_records(manifest, "hook")?
            .into_iter()
            .find(|record| {
                record.source == "hook" && record.source_native_id == input.source_native_id
            })
        {
            return Ok(existing
                .fact
                .get("context_generation")
                .and_then(Value::as_u64)
                .unwrap_or(0));
        }
        let session = index
            .sessions
            .entry(input.session_id.clone())
            .or_insert_with(|| RunAssetSessionIndex {
                session_id: input.session_id.clone(),
                context_generation: 0,
                relations: Vec::new(),
            });
        let kind = HookFactKind::parse(&input.hook);
        if kind == HookFactKind::CompactionFinished {
            session.context_generation = session.context_generation.saturating_add(1);
        }
        let context_generation = session.context_generation;
        append_record_with_index(
            manifest,
            RecordInput {
                stream: "hook",
                source: "hook",
                source_native_id: input.source_native_id,
                run_id: Some(manifest.run_id.clone()),
                activation_id: input.activation_id.clone(),
                session_id: Some(input.session_id.clone()),
                wall_time_ms: now_ms,
                source_sequence: None,
                causal_id: input.causal_id,
                correlation_id: input.correlation_id,
                fact: json!({
                    "hook": input.hook,
                    "session_id": input.session_id,
                    "activation_id": input.activation_id,
                    "context_generation": context_generation,
                    "payload": input.payload,
                }),
            },
            index,
        )?;
        Ok(context_generation)
    })
}

pub(super) fn record_machine_input(
    manifest: &mut RunAssetManifest,
    role: &str,
    record: &MachineInputRecord,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    with_record_index(manifest, |index| {
        append_machine_input_ledger_record(manifest, record)?;
        append_record_with_index(
            manifest,
            RecordInput {
                stream: "tmux",
                source: "machine_input",
                source_native_id: format!("machine_input:{}", record.transaction_id),
                run_id: Some(record.run_id.clone()),
                activation_id: Some(record.activation_id.clone()),
                session_id: None,
                wall_time_ms: now_ms,
                source_sequence: None,
                causal_id: None,
                correlation_id: Some(record.transaction_id.clone()),
                fact: json!({
                    "role": role,
                    "machine_input": record,
                }),
            },
            index,
        )?;
        let ledger = scan_machine_input_ledger(manifest, StreamReadMode::Strict)?;
        index_machine_input_ledger(
            index,
            ledger.record_count,
            ledger.record_count,
            ledger.latest_wall_time_ms.unwrap_or(now_ms),
        );
        Ok(())
    })
}

pub(super) fn record_qos_intent(
    manifest: &mut RunAssetManifest,
    qos: &flow::FlowQosIntent,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    append_record(
        manifest,
        RecordInput {
            stream: "qos",
            source: "run_assets",
            source_native_id: "qos:run_intent".to_string(),
            run_id: Some(manifest.run_id.clone()),
            activation_id: None,
            session_id: None,
            wall_time_ms: now_ms,
            source_sequence: None,
            causal_id: None,
            correlation_id: None,
            fact: json!({
                "qos": {
                    "urgency": qos_urgency_name(qos.urgency),
                    "completion_target": qos.completion_target,
                },
            }),
        },
    )
}

pub(super) fn record_topology_decision(
    manifest: &mut RunAssetManifest,
    input: TopologyDecisionInput,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    append_record(
        manifest,
        RecordInput {
            stream: "topology",
            source: input.source,
            source_native_id: input.source_native_id,
            run_id: Some(manifest.run_id.clone()),
            activation_id: None,
            session_id: None,
            wall_time_ms: now_ms,
            source_sequence: None,
            causal_id: input.causal_id,
            correlation_id: input.correlation_id,
            fact: input.fact,
        },
    )
}

pub(super) fn record_runtime_event(
    manifest: &mut RunAssetManifest,
    event: &runtime::Event,
    now_ms: u64,
) -> Result<(), RunAssetError> {
    let stream = runtime_event_stream(event.kind);
    append_record(
        manifest,
        RecordInput {
            stream,
            source: "runtime",
            source_native_id: format!("runtime_event:{}", event.sequence),
            run_id: event.source.run_id.clone(),
            activation_id: event.source.activation_id.clone(),
            session_id: None,
            wall_time_ms: now_ms,
            source_sequence: Some(event.sequence),
            causal_id: None,
            correlation_id: event.correlation.clone(),
            fact: json!({
                "event": runtime_event_json(event),
            }),
        },
    )
}

fn append_record(manifest: &mut RunAssetManifest, input: RecordInput) -> Result<(), RunAssetError> {
    with_record_index(manifest, |index| {
        append_record_with_index(manifest, input, index)
    })
}

fn append_record_with_index(
    manifest: &RunAssetManifest,
    input: RecordInput,
    index: &mut RunAssetRecordIndex,
) -> Result<(), RunAssetError> {
    let existing_records = read_stream_records(manifest, input.stream)?;
    if existing_records
        .iter()
        .any(|record| record_matches_input(record, &input))
    {
        return Ok(());
    }
    let sequence = input
        .source_sequence
        .unwrap_or_else(|| next_sequence(index, input.stream));
    let record = DurableRecord {
        record_id: format!("rec-{}-{sequence:020}", input.stream),
        run_id: input.run_id,
        activation_id: input.activation_id,
        session_id: input.session_id,
        source: input.source.to_string(),
        source_native_id: input.source_native_id,
        wall_time_ms: input.wall_time_ms,
        source_sequence: sequence,
        causal_id: input.causal_id,
        correlation_id: input.correlation_id,
        fact: input.fact,
    };
    let relative_path = format!("{RECORD_ROOT}/{}.jsonl", input.stream);
    let path = manifest.root.join(&relative_path);
    append_jsonl_record(&path, &record)?;
    index_record_file(
        index,
        input.stream,
        &relative_path,
        sequence,
        input.wall_time_ms,
    );
    Ok(())
}

fn index_machine_input_ledger(
    index: &mut RunAssetRecordIndex,
    latest_sequence: u64,
    record_count: u64,
    now_ms: u64,
) {
    let file = index
        .files
        .entry("machine_input_ledger".to_string())
        .or_insert_with(|| record_file("machine-inputs.jsonl"));
    file.relative_path = "machine-inputs.jsonl".to_string();
    file.latest_sequence = latest_sequence;
    file.record_count = record_count;
    file.latest_wall_time_ms = now_ms;
}

fn append_jsonl_record(path: &Path, record: &DurableRecord) -> Result<(), RunAssetError> {
    let line = serde_json::to_vec(record)
        .map_err(|err| RunAssetError::new(format!("serialize durable record failed: {err}")))?;
    let mut payload = line;
    payload.push(b'\n');
    append_private_line(path, &payload).map_err(|err| {
        RunAssetError::new(format!(
            "append durable record file {} failed: {err}",
            path.display()
        ))
    })
}

fn append_machine_input_ledger_record(
    manifest: &RunAssetManifest,
    record: &MachineInputRecord,
) -> Result<(), RunAssetError> {
    let path = manifest.root.join("machine-inputs.jsonl");
    let records = read_machine_input_ledger_records(manifest, StreamReadMode::RecoverTornTail)?;
    if records
        .iter()
        .any(|existing| machine_input_records_match(existing, record))
    {
        return Ok(());
    }
    append_machine_input_ledger_payload(&path, record)
}

pub(super) fn append_machine_input_ledger_direct(
    path: &Path,
    record: &MachineInputRecord,
) -> Result<(), RunAssetError> {
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    create_dir_all(&root.join(RECORD_ROOT))?;
    ensure_private_dir(&root.join(RECORD_ROOT))?;
    let _lock = RecordStoreLock::acquire_root(root)?;
    let records =
        read_machine_input_ledger_records_at(root, path, StreamReadMode::RecoverTornTail)?;
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
    with_record_index(manifest, |index| Ok(index.clone()))
}

pub(super) fn record_index(
    manifest: &RunAssetManifest,
) -> Result<RunAssetRecordIndex, RunAssetError> {
    rebuild_index_from_streams(manifest)
}

fn with_record_index<T>(
    manifest: &RunAssetManifest,
    action: impl FnOnce(&mut RunAssetRecordIndex) -> Result<T, RunAssetError>,
) -> Result<T, RunAssetError> {
    create_dir_all(&manifest.root.join(RECORD_ROOT))?;
    ensure_private_dir(&manifest.root.join(RECORD_ROOT))?;
    let _lock = RecordStoreLock::acquire(manifest)?;
    let mut index =
        rebuild_index_from_streams_with_mode(manifest, StreamReadMode::RecoverTornTail)?;
    let result = action(&mut index)?;
    write_record_index(manifest, &index)?;
    Ok(result)
}

fn rebuild_index_from_streams(
    manifest: &RunAssetManifest,
) -> Result<RunAssetRecordIndex, RunAssetError> {
    rebuild_index_from_streams_with_mode(manifest, StreamReadMode::Strict)
}

fn rebuild_index_from_streams_with_mode(
    manifest: &RunAssetManifest,
    mode: StreamReadMode,
) -> Result<RunAssetRecordIndex, RunAssetError> {
    let mut index = RunAssetRecordIndex::default();
    let records_dir = manifest.root.join(RECORD_ROOT);
    match fs::read_dir(&records_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|err| {
                    RunAssetError::new(format!(
                        "read durable records directory {} failed: {err}",
                        records_dir.display()
                    ))
                })?;
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(stream) = path.file_stem().and_then(|name| name.to_str()) else {
                    continue;
                };
                for record in read_stream_records_with_mode(manifest, stream, mode)? {
                    let relative_path = format!("{RECORD_ROOT}/{stream}.jsonl");
                    index_record_file(
                        &mut index,
                        stream,
                        &relative_path,
                        record.source_sequence,
                        record.wall_time_ms,
                    );
                    index_record_session_fact(&mut index, &record);
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(RunAssetError::new(format!(
                "read durable records directory {} failed: {err}",
                records_dir.display()
            )));
        }
    }
    let ledger = scan_machine_input_ledger(manifest, mode)?;
    if ledger.record_count > 0 {
        index_machine_input_ledger(
            &mut index,
            ledger.record_count,
            ledger.record_count,
            ledger.latest_wall_time_ms.unwrap_or(0),
        );
    }
    Ok(index)
}

fn write_record_index(
    manifest: &RunAssetManifest,
    index: &RunAssetRecordIndex,
) -> Result<(), RunAssetError> {
    let path = manifest.root.join(RECORD_INDEX_RELATIVE_PATH);
    let bytes = serde_json::to_vec_pretty(index)
        .map_err(|err| RunAssetError::new(format!("serialize record index failed: {err}")))?;
    let mut bytes = bytes;
    bytes.push(b'\n');
    atomic_write_private(&path, &bytes).map_err(|err| {
        RunAssetError::new(format!(
            "write durable record index {} failed: {err}",
            path.display()
        ))
    })
}

fn read_stream_records(
    manifest: &RunAssetManifest,
    stream: &str,
) -> Result<Vec<DurableRecord>, RunAssetError> {
    read_stream_records_with_mode(manifest, stream, StreamReadMode::Strict)
}

fn read_stream_records_with_mode(
    manifest: &RunAssetManifest,
    stream: &str,
    mode: StreamReadMode,
) -> Result<Vec<DurableRecord>, RunAssetError> {
    let path = manifest.root.join(format!("{RECORD_ROOT}/{stream}.jsonl"));
    let Some(bytes) = read_regular_private(&path).map_err(|err| {
        RunAssetError::new(format!(
            "read durable record file {} failed: {err}",
            path.display()
        ))
    })?
    else {
        return Ok(Vec::new());
    };
    if mode == StreamReadMode::RecoverTornTail && !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return recover_torn_tail(manifest, stream, &path, &bytes);
    }
    parse_stream_records_bytes(&path, &bytes)
}

fn recover_torn_tail(
    manifest: &RunAssetManifest,
    stream: &str,
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<DurableRecord>, RunAssetError> {
    let prefix_end = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let (committed, torn_tail) = bytes.split_at(prefix_end);
    let records = parse_stream_records_bytes(path, committed)?;
    quarantine_torn_tail(manifest, stream, torn_tail)?;
    atomic_write_private(path, committed).map_err(|err| {
        RunAssetError::new(format!(
            "recover durable record file {} failed: {err}",
            path.display()
        ))
    })?;
    Ok(records)
}

fn quarantine_torn_tail(
    manifest: &RunAssetManifest,
    stream: &str,
    torn_tail: &[u8],
) -> Result<(), RunAssetError> {
    quarantine_torn_tail_at(&manifest.root, stream, torn_tail)
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

fn parse_stream_records_bytes(
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<DurableRecord>, RunAssetError> {
    let mut records = Vec::new();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        records.push(serde_json::from_slice(line).map_err(|err| {
            RunAssetError::new(format!(
                "parse durable record file {} line {} failed: {err}",
                path.display(),
                index + 1
            ))
        })?);
    }
    Ok(records)
}

fn stable_tail_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn record_matches_input(record: &DurableRecord, input: &RecordInput) -> bool {
    record.source == input.source
        && record.source_native_id == input.source_native_id
        && input
            .source_sequence
            .map(|sequence| record.source_sequence == sequence)
            .unwrap_or(true)
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

fn index_record_session_fact(index: &mut RunAssetRecordIndex, record: &DurableRecord) {
    match record.source_native_id.as_str() {
        source if source.starts_with("session:") => {
            let Some(session_id) = record.session_id.as_deref() else {
                return;
            };
            let relation = record
                .fact
                .get("relation")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if relation.is_empty() {
                return;
            }
            let activation_id = record
                .fact
                .get("activation_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            index_session_relation_by_name(
                index,
                session_id,
                relation,
                record.run_id.as_deref().unwrap_or_default(),
                activation_id,
                record.wall_time_ms,
            );
        }
        _ if record.source == "hook" => {
            let Some(session_id) = record.session_id.as_deref() else {
                return;
            };
            if let Some(generation) = record
                .fact
                .get("context_generation")
                .and_then(Value::as_u64)
            {
                let session = index
                    .sessions
                    .entry(session_id.to_string())
                    .or_insert_with(|| RunAssetSessionIndex {
                        session_id: session_id.to_string(),
                        context_generation: 0,
                        relations: Vec::new(),
                    });
                session.context_generation = session.context_generation.max(generation);
            }
        }
        _ => {}
    }
}

fn index_session_relation(
    index: &mut RunAssetRecordIndex,
    manifest: &RunAssetManifest,
    session_id: &str,
    relation: SessionRelation,
    activation_id: Option<&str>,
    now_ms: u64,
) {
    index_session_relation_by_name(
        index,
        session_id,
        relation.as_str(),
        &manifest.run_id,
        activation_id.map(str::to_string),
        now_ms,
    );
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

#[derive(Debug, Clone, Copy)]
struct LedgerScan {
    record_count: u64,
    latest_wall_time_ms: Option<u64>,
}

fn read_machine_input_ledger_records(
    manifest: &RunAssetManifest,
    mode: StreamReadMode,
) -> Result<Vec<MachineInputRecord>, RunAssetError> {
    let path = manifest.root.join("machine-inputs.jsonl");
    read_machine_input_ledger_records_at(&manifest.root, &path, mode)
}

fn read_machine_input_ledger_records_at(
    root: &Path,
    path: &Path,
    mode: StreamReadMode,
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
    if mode == StreamReadMode::RecoverTornTail && !bytes.is_empty() && !bytes.ends_with(b"\n") {
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

fn scan_machine_input_ledger(
    manifest: &RunAssetManifest,
    mode: StreamReadMode,
) -> Result<LedgerScan, RunAssetError> {
    let records = read_machine_input_ledger_records(manifest, mode)?;
    let record_count = records.len() as u64;
    let latest_wall_time_ms = records.iter().map(|record| record.submitted_at_ms).max();
    Ok(LedgerScan {
        record_count,
        latest_wall_time_ms,
    })
}

struct RecordStoreLock {
    #[allow(dead_code)]
    file: fs::File,
}

impl RecordStoreLock {
    fn acquire(manifest: &RunAssetManifest) -> Result<Self, RunAssetError> {
        Self::acquire_root(&manifest.root)
    }

    fn acquire_root(root: &Path) -> Result<Self, RunAssetError> {
        let path = root.join(RECORD_WRITER_LOCK_RELATIVE_PATH);
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
            ensure_private_dir(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options.open(&path).map_err(|err| {
            RunAssetError::new(format!(
                "open record store writer lock {} failed: {err}",
                path.display()
            ))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                return Err(RunAssetError::new(format!(
                    "record store writer lock {} is already held: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                )));
            }
        }
        Ok(Self { file })
    }
}

fn next_sequence(index: &RunAssetRecordIndex, stream: &str) -> u64 {
    index
        .files
        .get(stream)
        .map(|file| file.latest_sequence.saturating_add(1))
        .unwrap_or(1)
}

fn runtime_event_stream(kind: runtime::EventKind) -> &'static str {
    match kind {
        runtime::EventKind::ArtifactDelivered
        | runtime::EventKind::BoardPatched
        | runtime::EventKind::EffectRecorded => "delivery",
        runtime::EventKind::FlowApplied | runtime::EventKind::FlowUpdate => "flow",
        runtime::EventKind::StopDecision | runtime::EventKind::StopObserved => "outcome",
        runtime::EventKind::ActivationStatusChanged
        | runtime::EventKind::NodeActivated
        | runtime::EventKind::RunStarted
        | runtime::EventKind::RunStatusChanged => "runtime",
    }
}

fn runtime_event_json(event: &runtime::Event) -> Value {
    json!({
        "sequence": event.sequence,
        "kind": event_kind_name(event.kind),
        "strength": event_strength_name(event.strength),
        "actor": event.actor,
        "source": {
            "run_id": event.source.run_id,
            "activation_id": event.source.activation_id,
            "source_id": event.source.source_id,
        },
        "payload": runtime_payload_json(&event.payload),
    })
}

fn runtime_payload_json(payload: &runtime::EventPayload) -> Value {
    match payload {
        runtime::EventPayload::RunStarted { run_id } => json!({ "run_id": run_id }),
        runtime::EventPayload::RunStatusChanged { run_id, status } => {
            json!({ "run_id": run_id, "status": run_status_name(*status) })
        }
        runtime::EventPayload::NodeActivated {
            run_id,
            activation_id,
            node_id,
            stable_key,
            context,
            stop_contract,
            flow_lock_mode,
            flow_lock_id,
            contract_hash,
        } => json!({
            "run_id": run_id,
            "activation_id": activation_id,
            "node_id": node_id,
            "stable_key": stable_key,
            "context": context,
            "required_artifacts": stop_contract.required_artifacts(),
            "required_effects": stop_contract.required_effects(),
            "flow_lock_mode": flow_lock_mode.map(flow_lock_mode_name),
            "flow_lock_id": flow_lock_id,
            "contract_hash": contract_hash,
        }),
        runtime::EventPayload::ActivationStatusChanged {
            run_id,
            activation_id,
            status,
        } => json!({
            "run_id": run_id,
            "activation_id": activation_id,
            "status": activation_status_name(*status),
        }),
        runtime::EventPayload::ArtifactDelivered {
            run_id,
            activation_id,
            artifact_id,
            artifact_key,
            content_hash,
            payload,
        } => json!({
            "run_id": run_id,
            "activation_id": activation_id,
            "artifact_id": artifact_id,
            "artifact_key": artifact_key,
            "content_hash": content_hash,
            "payload": payload,
        }),
        runtime::EventPayload::BoardPatched {
            run_id,
            activation_id,
            key,
            value,
            version,
        } => json!({
            "run_id": run_id,
            "activation_id": activation_id,
            "key": key,
            "value": value,
            "version": version,
        }),
        runtime::EventPayload::StopObserved {
            run_id,
            activation_id,
            observation,
        } => json!({
            "run_id": run_id,
            "activation_id": activation_id,
            "observation": {
                "reason": observation.reason,
            },
        }),
        runtime::EventPayload::StopDecision {
            run_id,
            activation_id,
            decision,
        } => json!({
            "run_id": run_id,
            "activation_id": activation_id,
            "decision": {
                "kind": stop_decision_kind_name(decision.kind),
                "attempt": decision.attempt,
                "missing_artifacts": decision.missing_artifacts,
                "missing_effects": decision.missing_effects,
                "reason": decision.reason,
            },
        }),
        runtime::EventPayload::EffectRecorded {
            run_id,
            activation_id,
            effect_key,
            payload,
        } => json!({
            "run_id": run_id,
            "activation_id": activation_id,
            "effect_key": effect_key,
            "payload": payload,
        }),
        runtime::EventPayload::FlowApplied {
            run_id,
            mode,
            lock_id,
            content_hash,
        } => json!({
            "run_id": run_id,
            "mode": flow_lock_mode_name(*mode),
            "lock_id": lock_id,
            "content_hash": content_hash,
        }),
        runtime::EventPayload::FlowUpdate {
            run_id,
            status,
            mode,
            lock_id,
            contract_hash,
        } => json!({
            "run_id": run_id,
            "status": flow_update_status_name(*status),
            "mode": flow_lock_mode_name(*mode),
            "lock_id": lock_id,
            "contract_hash": contract_hash,
        }),
    }
}

fn event_kind_name(kind: runtime::EventKind) -> &'static str {
    match kind {
        runtime::EventKind::ActivationStatusChanged => "activation_status_changed",
        runtime::EventKind::ArtifactDelivered => "artifact_delivered",
        runtime::EventKind::BoardPatched => "board_patched",
        runtime::EventKind::EffectRecorded => "effect_recorded",
        runtime::EventKind::FlowApplied => "flow_applied",
        runtime::EventKind::FlowUpdate => "flow_update",
        runtime::EventKind::NodeActivated => "node_activated",
        runtime::EventKind::RunStarted => "run_started",
        runtime::EventKind::RunStatusChanged => "run_status_changed",
        runtime::EventKind::StopDecision => "stop_decision",
        runtime::EventKind::StopObserved => "stop_observed",
    }
}

fn event_strength_name(strength: runtime::EventStrength) -> &'static str {
    match strength {
        runtime::EventStrength::Applied => "applied",
        runtime::EventStrength::Checked => "checked",
        runtime::EventStrength::Decision => "decision",
        runtime::EventStrength::Observed => "observed",
        runtime::EventStrength::Proposed => "proposed",
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

fn activation_status_name(status: runtime::ActivationStatus) -> &'static str {
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

fn flow_lock_mode_name(mode: runtime::FlowLockMode) -> &'static str {
    match mode {
        runtime::FlowLockMode::FutureActivations => "future_activations",
        runtime::FlowLockMode::CheckpointRestart => "checkpoint_restart",
    }
}

fn flow_update_status_name(status: runtime::FlowUpdateStatus) -> &'static str {
    match status {
        runtime::FlowUpdateStatus::Proposed => "proposed",
        runtime::FlowUpdateStatus::Checked => "checked",
        runtime::FlowUpdateStatus::Applied => "applied",
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

fn non_empty_string(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}
