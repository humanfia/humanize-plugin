use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::public_event::{
    PublicEventKind, PublicEventPayload, PublicRefMapper, PublicSource,
    validate_known_wire_payload, wire_data,
};
use super::{
    RunAssetError, RunAssetManifest, append_private_line, atomic_write_private, create_dir_all,
    ensure_private_dir, read_regular_private, truncate_private, write_create_new_private,
};

pub(super) const EVENT_SCHEMA_NAME: &str = "humanize.public_journal.event";
pub(super) const JOURNAL_SCHEMA_NAME: &str = "humanize.public_journal";
pub(super) const JOURNAL_SCHEMA_MAJOR: u32 = 1;
pub(super) const EVENT_LOG_RELATIVE_PATH: &str = "records/events.jsonl";
pub(super) const SEAL_RELATIVE_PATH: &str = "records/journal-seal.json";

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct PublicJournalEvent {
    pub schema_name: String,
    pub schema_major: u32,
    pub seq: u64,
    pub event_id: String,
    pub occurred_at_ms: u64,
    pub run_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_ref: Option<String>,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_ref: Option<String>,
    pub data: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PublicJournalInput {
    pub kind: PublicEventKind,
    pub run_id: String,
    pub activation_id: Option<String>,
    pub session_id: Option<String>,
    pub revision_id: Option<String>,
    pub caused_by_seq: Option<u64>,
    pub correlation_id: Option<String>,
    pub source: PublicSource,
    pub source_native_id: String,
    pub payload: PublicEventPayload,
}

impl PublicJournalInput {
    fn source_ref(&self) -> &str {
        &self.source_native_id
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct PublicJournalAppend {
    pub seq: u64,
    pub appended: bool,
}

#[derive(Debug, Clone)]
pub(super) struct PreparedPublicJournalEvent {
    event: PublicJournalEvent,
    line: Vec<u8>,
    existing_seq: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedPublicJournalBatch {
    events: Vec<PreparedPublicJournalEvent>,
    expected_last_seq: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum JournalReadMode {
    Strict,
    RecoverTornTail,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct JournalSeal {
    schema_name: String,
    schema_major: u32,
    final_sha256: String,
    event_count: u64,
    last_seq: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct JournalDiskFingerprint {
    byte_len: u64,
    modified_at: Option<SystemTime>,
}

#[derive(Debug)]
struct ValidatedJournalState {
    fingerprint: JournalDiskFingerprint,
    byte_len: u64,
    event_count: u64,
    last_seq: u64,
    hasher: Sha256,
    events_by_id: BTreeMap<String, EventIdentity>,
    compaction_generation_by_session: BTreeMap<String, u64>,
    projection: PublicManifestProjection,
    sealable_run_started: bool,
    terminal_run_completed: bool,
}

#[derive(Debug, Default)]
struct PublicManifestProjection {
    run_ref: Option<String>,
    created_at_ms: Option<u64>,
    updated_at_ms: Option<u64>,
    mode: Option<String>,
    status: Option<String>,
    revisions: BTreeMap<String, Value>,
    activations: BTreeMap<String, Value>,
    sessions: BTreeMap<String, Value>,
    facts: BTreeMap<String, Value>,
    artifacts: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct EventIdentity {
    seq: u64,
    semantic_sha256: String,
    context_generation: Option<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct JournalStateView {
    event_count: u64,
    last_seq: u64,
    current_sha256: String,
    sealable_run_started: bool,
    terminal_run_completed: bool,
}

impl ValidatedJournalState {
    fn from_events(
        fingerprint: JournalDiskFingerprint,
        bytes: &[u8],
        events: Vec<PublicJournalEvent>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let mut state = Self {
            fingerprint,
            byte_len: bytes.len() as u64,
            event_count: 0,
            last_seq: 0,
            hasher,
            events_by_id: BTreeMap::new(),
            compaction_generation_by_session: BTreeMap::new(),
            projection: PublicManifestProjection::default(),
            sealable_run_started: false,
            terminal_run_completed: false,
        };
        for event in events {
            state.observe_event(&event);
            state.event_count = state.event_count.saturating_add(1);
            state.last_seq = event.seq;
            state.events_by_id.insert(
                event.event_id.clone(),
                EventIdentity {
                    seq: event.seq,
                    semantic_sha256: event_semantic_sha256(&event),
                    context_generation: event_context_generation(&event),
                },
            );
        }
        state
    }

    fn empty(fingerprint: JournalDiskFingerprint) -> Self {
        Self::from_events(fingerprint, &[], Vec::new())
    }

    fn current_sha256(&self) -> String {
        format!("sha256:{:x}", self.hasher.clone().finalize())
    }

    fn view(&self) -> JournalStateView {
        JournalStateView {
            event_count: self.event_count,
            last_seq: self.last_seq,
            current_sha256: self.current_sha256(),
            sealable_run_started: self.sealable_run_started,
            terminal_run_completed: self.terminal_run_completed,
        }
    }

    fn apply_append(
        &mut self,
        event: PublicJournalEvent,
        line: &[u8],
        fingerprint: JournalDiskFingerprint,
    ) {
        self.hasher.update(line);
        self.byte_len = self.byte_len.saturating_add(line.len() as u64);
        self.fingerprint = fingerprint;
        self.observe_event(&event);
        self.event_count = self.event_count.saturating_add(1);
        self.last_seq = event.seq;
        self.events_by_id.insert(
            event.event_id.clone(),
            EventIdentity {
                seq: event.seq,
                semantic_sha256: event_semantic_sha256(&event),
                context_generation: event_context_generation(&event),
            },
        );
    }

    fn observe_event(&mut self, event: &PublicJournalEvent) {
        self.projection.observe(event);
        if event.kind == PublicEventKind::RunStarted.as_str()
            && event
                .data
                .pointer("/payload/mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| matches!(mode, "finite" | "manual"))
        {
            self.sealable_run_started = true;
        }
        if event.kind == PublicEventKind::RunCompleted.as_str()
            && event
                .data
                .pointer("/payload/status")
                .and_then(Value::as_str)
                .is_some_and(|status| matches!(status, "completed" | "stopped" | "failed"))
        {
            self.terminal_run_completed = true;
        }
        if let (Some(session_ref), Some(generation)) =
            (event.session_ref.as_ref(), event_context_generation(event))
        {
            self.compaction_generation_by_session
                .entry(session_ref.clone())
                .and_modify(|current| *current = (*current).max(generation))
                .or_insert(generation);
        }
    }
}

impl PublicManifestProjection {
    fn observe(&mut self, event: &PublicJournalEvent) {
        self.run_ref.get_or_insert_with(|| event.run_ref.clone());
        self.created_at_ms.get_or_insert(event.occurred_at_ms);
        self.updated_at_ms = Some(event.occurred_at_ms);
        let payload = event.data.get("payload").cloned().unwrap_or(Value::Null);
        match PublicEventKind::parse(&event.kind) {
            Some(PublicEventKind::RunStarted)
            | Some(PublicEventKind::RunStatus)
            | Some(PublicEventKind::RunCompleted) => {
                if let Some(mode) = payload.get("mode").and_then(Value::as_str) {
                    self.mode = Some(mode.to_string());
                }
                if let Some(status) = payload.get("status").and_then(Value::as_str) {
                    self.status = Some(status.to_string());
                }
            }
            Some(PublicEventKind::FlowRevisionPrepared)
            | Some(PublicEventKind::FlowRevisionApplied)
            | Some(PublicEventKind::FlowRevisionRejected) => {
                self.revisions.insert(event.event_id.clone(), payload);
            }
            Some(PublicEventKind::ActivationCreated)
            | Some(PublicEventKind::ActivationStatus)
            | Some(PublicEventKind::ActivationCompleted) => {
                if let Some(reference) = event.activation_ref.as_ref() {
                    self.activations.insert(reference.clone(), payload);
                }
            }
            Some(PublicEventKind::AgentSessionStarted)
            | Some(PublicEventKind::AgentSessionBound)
            | Some(PublicEventKind::AgentSessionEnded) => {
                if let Some(reference) = event.session_ref.as_ref() {
                    self.sessions.insert(reference.clone(), payload);
                }
            }
            Some(PublicEventKind::FactRecorded) => {
                self.facts.insert(event.event_id.clone(), payload);
            }
            Some(PublicEventKind::ArtifactRecorded) => {
                self.artifacts.insert(event.event_id.clone(), payload);
            }
            _ => {}
        }
    }

    fn to_value(&self, journal: Value) -> Value {
        json!({
            "schema_name": "humanize.public_manifest",
            "schema_major": JOURNAL_SCHEMA_MAJOR,
            "run_ref": self.run_ref,
            "created_at_ms": self.created_at_ms,
            "updated_at_ms": self.updated_at_ms,
            "mode": self.mode,
            "status": self.status.as_deref().unwrap_or("pending"),
            "journal": journal,
            "flow": {
                "revisions": self.revisions.values().collect::<Vec<_>>(),
            },
            "activations": self.activations,
            "sessions": self.sessions,
            "facts": self.facts.values().collect::<Vec<_>>(),
            "artifacts": self.artifacts.values().collect::<Vec<_>>(),
        })
    }
}

static JOURNAL_WRITER_STATES: OnceLock<Mutex<BTreeMap<PathBuf, ValidatedJournalState>>> =
    OnceLock::new();
static PUBLIC_REF_SALTS: OnceLock<Mutex<BTreeMap<PathBuf, Vec<u8>>>> = OnceLock::new();

#[cfg(test)]
static JOURNAL_FULL_SCANS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static JOURNAL_FULL_SCANS_BY_ROOT: OnceLock<Mutex<BTreeMap<PathBuf, usize>>> = OnceLock::new();

#[cfg(test)]
fn note_journal_full_scan(root: &Path) {
    JOURNAL_FULL_SCANS.fetch_add(1, Ordering::Relaxed);
    let mut scans = JOURNAL_FULL_SCANS_BY_ROOT
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    *scans.entry(root.to_path_buf()).or_default() += 1;
}

#[cfg(test)]
fn append_event(
    manifest: &RunAssetManifest,
    input: PublicJournalInput,
    occurred_at_ms: u64,
) -> Result<PublicJournalAppend, RunAssetError> {
    append_prepared_event_batch(
        manifest,
        prepare_event_batch(manifest, vec![(input, occurred_at_ms)])?,
    )?
    .into_iter()
    .next()
    .ok_or_else(|| RunAssetError::new("public journal append produced no result"))
}

fn materialize_event(
    input: PublicJournalInput,
    occurred_at_ms: u64,
    seq: u64,
    salt: &[u8],
) -> Result<PublicJournalEvent, RunAssetError> {
    if !input.payload.matches_kind(input.kind) {
        return Err(RunAssetError::new(format!(
            "public journal {} payload variant does not match its event kind",
            input.kind.as_str()
        )));
    }
    let kind = input.kind;
    let event_id = stable_event_id(salt, &input.run_id, kind.as_str(), input.source_ref());
    let public_ref = |namespace: &str, value: &str| stable_public_ref(salt, namespace, value);
    let refs = PublicRefMapper::new(&public_ref);
    let source_ref = public_ref("source", &input.source_native_id);
    let event = PublicJournalEvent {
        schema_name: EVENT_SCHEMA_NAME.to_string(),
        schema_major: JOURNAL_SCHEMA_MAJOR,
        seq,
        event_id,
        occurred_at_ms,
        run_ref: public_ref("run", &input.run_id),
        activation_ref: input
            .activation_id
            .as_deref()
            .map(|value| public_ref("activation", value)),
        session_ref: input
            .session_id
            .as_deref()
            .map(|value| public_ref("session", value)),
        revision_ref: input
            .revision_id
            .as_deref()
            .map(|value| public_ref("revision", value)),
        kind: kind.as_str().to_string(),
        caused_by_seq: input.caused_by_seq,
        correlation_ref: input
            .correlation_id
            .as_deref()
            .map(|value| public_ref("correlation", value)),
        data: wire_data(input.source, source_ref, &input.payload, &refs),
        extra: BTreeMap::new(),
    };
    validate_known_wire_payload(kind, &event.data).map_err(|err| {
        RunAssetError::new(format!(
            "public journal {} payload is malformed: {err}",
            kind.as_str()
        ))
    })?;
    Ok(event)
}

pub(crate) fn prepare_event_batch(
    manifest: &RunAssetManifest,
    inputs: Vec<(PublicJournalInput, u64)>,
) -> Result<PreparedPublicJournalBatch, RunAssetError> {
    create_dir_all(&manifest.root.join("records"))?;
    ensure_private_dir(&manifest.root.join("records"))?;
    let salt = public_ref_salt()?;
    let mut states = journal_writer_states()
        .lock()
        .map_err(|_| RunAssetError::new("public journal writer state lock is poisoned"))?;
    let state = state_for_root_locked(
        &mut states,
        &manifest.root,
        JournalReadMode::RecoverTornTail,
    )?;
    let seal = read_seal(&manifest.root)?;
    let sealed = seal
        .as_ref()
        .is_some_and(|seal| seal_matches_state(seal, &state.view()));
    if seal.is_some() && !sealed {
        return Err(RunAssetError::new(
            "public journal seal is corrupt and rejects publication",
        ));
    }
    let expected_last_seq = state.last_seq;
    let mut next_seq = expected_last_seq;
    let mut batch_identities = BTreeMap::<String, EventIdentity>::new();
    let mut prepared = Vec::with_capacity(inputs.len());
    for (input, occurred_at_ms) in inputs {
        let candidate_seq = next_seq.saturating_add(1);
        let event = materialize_event(input, occurred_at_ms, candidate_seq, &salt)?;
        let semantic_sha256 = event_semantic_sha256(&event);
        let existing = state
            .events_by_id
            .get(&event.event_id)
            .or_else(|| batch_identities.get(&event.event_id));
        let existing_seq = match existing {
            Some(existing) if existing.semantic_sha256 == semantic_sha256 => Some(existing.seq),
            Some(_) => {
                return Err(RunAssetError::new(format!(
                    "public journal idempotent retry conflicts with event {}",
                    event.event_id
                )));
            }
            None => {
                if sealed {
                    return Err(RunAssetError::new(
                        "public journal is sealed and rejects later appends",
                    ));
                }
                next_seq = candidate_seq;
                batch_identities.insert(
                    event.event_id.clone(),
                    EventIdentity {
                        seq: candidate_seq,
                        semantic_sha256,
                        context_generation: event_context_generation(&event),
                    },
                );
                None
            }
        };
        let mut line = serde_json::to_vec(&event).map_err(|err| {
            RunAssetError::new(format!("serialize public journal event failed: {err}"))
        })?;
        line.push(b'\n');
        prepared.push(PreparedPublicJournalEvent {
            event,
            line,
            existing_seq,
        });
    }
    Ok(PreparedPublicJournalBatch {
        events: prepared,
        expected_last_seq,
    })
}

pub(crate) fn append_prepared_event_batch(
    manifest: &RunAssetManifest,
    batch: PreparedPublicJournalBatch,
) -> Result<Vec<PublicJournalAppend>, RunAssetError> {
    if batch
        .events
        .iter()
        .all(|event| event.existing_seq.is_some())
    {
        return Ok(batch
            .events
            .into_iter()
            .map(|event| PublicJournalAppend {
                seq: event.existing_seq.expect("checked existing sequence"),
                appended: false,
            })
            .collect());
    }
    let mut states = journal_writer_states()
        .lock()
        .map_err(|_| RunAssetError::new("public journal writer state lock is poisoned"))?;
    let state = state_for_root_locked(
        &mut states,
        &manifest.root,
        JournalReadMode::RecoverTornTail,
    )?;
    reject_valid_seal_for_state(manifest, state)?;
    if state.last_seq != batch.expected_last_seq {
        return Err(RunAssetError::new(
            "public journal changed after batch preflight",
        ));
    }
    let mut results = Vec::with_capacity(batch.events.len());
    for prepared in batch.events {
        if let Some(seq) = prepared.existing_seq {
            results.push(PublicJournalAppend {
                seq,
                appended: false,
            });
            continue;
        }
        validate_next_event(state, &prepared.event)?;
        append_private_line(&manifest.root.join(EVENT_LOG_RELATIVE_PATH), &prepared.line).map_err(
            |err| {
                RunAssetError::new(format!(
                    "append public journal {} failed: {err}",
                    manifest.root.join(EVENT_LOG_RELATIVE_PATH).display()
                ))
            },
        )?;
        let fingerprint = disk_fingerprint(&manifest.root)?;
        let seq = prepared.event.seq;
        state.apply_append(prepared.event, &prepared.line, fingerprint);
        results.push(PublicJournalAppend {
            seq,
            appended: true,
        });
    }
    Ok(results)
}

pub(super) fn read_events(
    manifest: &RunAssetManifest,
    mode: JournalReadMode,
) -> Result<Vec<PublicJournalEvent>, RunAssetError> {
    read_events_at(&manifest.root, mode)
}

pub(super) fn read_events_at(
    root: &Path,
    mode: JournalReadMode,
) -> Result<Vec<PublicJournalEvent>, RunAssetError> {
    let path = root.join(EVENT_LOG_RELATIVE_PATH);
    let Some(bytes) = read_regular_private(&path).map_err(|err| {
        RunAssetError::new(format!(
            "read public journal {} failed: {err}",
            path.display()
        ))
    })?
    else {
        return Ok(Vec::new());
    };
    if mode == JournalReadMode::RecoverTornTail && !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return recover_torn_tail(root, &path, &bytes);
    }
    parse_events(&path, &bytes)
}

pub(super) fn seal_if_complete(manifest: &RunAssetManifest) -> Result<(), RunAssetError> {
    let state = journal_state(manifest, JournalReadMode::Strict)?;
    if !has_terminal_finite_runtime_evidence(manifest, &state) {
        return Ok(());
    }
    let seal_path = manifest.root.join(SEAL_RELATIVE_PATH);
    if read_seal(&manifest.root)?.is_some() {
        return Ok(());
    }
    if state.event_count == 0 {
        return Ok(());
    }
    let seal = JournalSeal {
        schema_name: JOURNAL_SCHEMA_NAME.to_string(),
        schema_major: JOURNAL_SCHEMA_MAJOR,
        final_sha256: state.current_sha256.clone(),
        event_count: state.event_count,
        last_seq: state.last_seq,
    };
    let mut bytes = serde_json::to_vec_pretty(&seal)
        .map_err(|err| RunAssetError::new(format!("serialize journal seal failed: {err}")))?;
    bytes.push(b'\n');
    atomic_write_private(&seal_path, &bytes).map_err(|err| {
        RunAssetError::new(format!(
            "write journal seal {} failed: {err}",
            seal_path.display()
        ))
    })
}

pub(super) fn manifest_summary(manifest: &RunAssetManifest) -> Result<Value, RunAssetError> {
    let state = journal_state(manifest, JournalReadMode::RecoverTornTail)?;
    let current_sha256 = state.current_sha256.clone();
    let seal = read_seal(&manifest.root)?;
    let has_quarantine = has_journal_quarantine(&manifest.root)?;
    let (status, final_sha256) = match seal {
        Some(seal) if has_quarantine || !seal_matches_state(&seal, &state) => {
            ("corrupt", Value::String(seal.final_sha256))
        }
        Some(seal) => ("sealed", Value::String(seal.final_sha256)),
        None if has_quarantine => ("corrupt", Value::Null),
        None => ("open", Value::Null),
    };
    Ok(json!({
        "schema_name": JOURNAL_SCHEMA_NAME,
        "schema_major": JOURNAL_SCHEMA_MAJOR,
        "path": EVENT_LOG_RELATIVE_PATH,
        "status": status,
        "event_count": state.event_count,
        "last_seq": state.last_seq,
        "current_sha256": current_sha256,
        "final_sha256": final_sha256
    }))
}

pub(super) fn reconcile_public_seal(
    manifest: &RunAssetManifest,
    private_terminal: bool,
    mutation: bool,
) -> Result<(), RunAssetError> {
    let state = journal_state(manifest, JournalReadMode::RecoverTornTail)?;
    let path = manifest.root.join(SEAL_RELATIVE_PATH);
    let bytes = match inspect_public_seal(&path)? {
        PublicSealEntry::Missing => return Ok(()),
        PublicSealEntry::Bytes(bytes) => bytes,
        PublicSealEntry::Unsafe(kind) => {
            quarantine_invalid_seal(manifest, kind.as_bytes())?;
            return Ok(());
        }
    };
    let seal = serde_json::from_slice::<JournalSeal>(&bytes).ok();
    let valid = seal
        .as_ref()
        .is_some_and(|seal| seal_matches_state(seal, &state));
    if valid && private_terminal {
        if mutation {
            return Err(RunAssetError::new(
                "public journal has a valid terminal seal and rejects mutation",
            ));
        }
        return Ok(());
    }
    quarantine_invalid_seal(manifest, &bytes)?;
    Ok(())
}

pub(super) fn public_manifest_projection(
    manifest: &RunAssetManifest,
) -> Result<Value, RunAssetError> {
    let journal = manifest_summary(manifest)?;
    let mut states = journal_writer_states()
        .lock()
        .map_err(|_| RunAssetError::new("public journal writer state lock is poisoned"))?;
    let state = state_for_root_locked(
        &mut states,
        &manifest.root,
        JournalReadMode::RecoverTornTail,
    )?;
    Ok(state.projection.to_value(journal))
}

pub(super) fn hook_context_generation(
    manifest: &RunAssetManifest,
    session_id: &str,
    source_native_id: &str,
    finishes_compaction: bool,
) -> Result<u64, RunAssetError> {
    let salt = public_ref_salt()?;
    let event_id = stable_event_id(
        &salt,
        &manifest.run_id,
        PublicEventKind::HookObserved.as_str(),
        source_native_id,
    );
    let session_ref = stable_public_ref(&salt, "session", session_id);
    let mut states = journal_writer_states()
        .lock()
        .map_err(|_| RunAssetError::new("public journal writer state lock is poisoned"))?;
    let state = state_for_root_locked(
        &mut states,
        &manifest.root,
        JournalReadMode::RecoverTornTail,
    )?;
    if let Some(existing) = state.events_by_id.get(&event_id)
        && let Some(generation) = existing.context_generation
    {
        return Ok(generation);
    }
    let current = state
        .compaction_generation_by_session
        .get(&session_ref)
        .copied()
        .unwrap_or(0);
    Ok(if finishes_compaction {
        current.saturating_add(1)
    } else {
        current
    })
}

fn validate_next_event(
    state: &ValidatedJournalState,
    event: &PublicJournalEvent,
) -> Result<(), RunAssetError> {
    if event.schema_major != JOURNAL_SCHEMA_MAJOR || event.schema_name != EVENT_SCHEMA_NAME {
        return Err(RunAssetError::new("public journal event schema mismatch"));
    }
    if event.seq != state.last_seq.saturating_add(1) {
        return Err(RunAssetError::new(
            "public journal sequence is not contiguous",
        ));
    }
    if state.events_by_id.contains_key(&event.event_id) {
        return Err(RunAssetError::new("public journal event id is not unique"));
    }
    Ok(())
}

fn reject_valid_seal_for_state(
    manifest: &RunAssetManifest,
    state: &ValidatedJournalState,
) -> Result<(), RunAssetError> {
    let Some(seal) = read_seal(&manifest.root)? else {
        return Ok(());
    };
    if seal_matches_state(&seal, &state.view()) && !has_journal_quarantine(&manifest.root)? {
        return Err(RunAssetError::new(
            "public journal is sealed and rejects later appends",
        ));
    }
    Err(RunAssetError::new(
        "public journal seal is corrupt and rejects later appends",
    ))
}

fn seal_matches_state(seal: &JournalSeal, state: &JournalStateView) -> bool {
    seal.schema_name == JOURNAL_SCHEMA_NAME
        && seal.schema_major == JOURNAL_SCHEMA_MAJOR
        && seal.final_sha256 == state.current_sha256
        && seal.event_count == state.event_count
        && seal.last_seq == state.last_seq
}

fn has_terminal_finite_runtime_evidence(
    manifest: &RunAssetManifest,
    state: &JournalStateView,
) -> bool {
    state.sealable_run_started
        && state.terminal_run_completed
        && public_projection_is_terminal(manifest)
}

fn public_projection_is_terminal(manifest: &RunAssetManifest) -> bool {
    manifest.activations.values().all(|activation| {
        activation.resource_cleanup_status != "owned"
            && !matches!(activation.capture_phase.as_str(), "starting" | "capturing")
            && !matches!(
                activation.preservation_status.as_str(),
                "starting" | "capturing"
            )
    })
}

fn journal_writer_states() -> &'static Mutex<BTreeMap<PathBuf, ValidatedJournalState>> {
    JOURNAL_WRITER_STATES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn journal_state(
    manifest: &RunAssetManifest,
    mode: JournalReadMode,
) -> Result<JournalStateView, RunAssetError> {
    let mut states = journal_writer_states()
        .lock()
        .map_err(|_| RunAssetError::new("public journal writer state lock is poisoned"))?;
    state_for_root_locked(&mut states, &manifest.root, mode).map(|state| state.view())
}

fn state_for_root_locked<'a>(
    states: &'a mut BTreeMap<PathBuf, ValidatedJournalState>,
    root: &Path,
    mode: JournalReadMode,
) -> Result<&'a mut ValidatedJournalState, RunAssetError> {
    let fingerprint = disk_fingerprint(root)?;
    let stale = states
        .get(root)
        .is_none_or(|state| state.fingerprint != fingerprint);
    if stale {
        let state = scan_journal_state(root, mode)?;
        states.insert(root.to_path_buf(), state);
    }
    states
        .get_mut(root)
        .ok_or_else(|| RunAssetError::new("public journal writer state is missing after rebuild"))
}

fn scan_journal_state(
    root: &Path,
    mode: JournalReadMode,
) -> Result<ValidatedJournalState, RunAssetError> {
    #[cfg(test)]
    note_journal_full_scan(root);

    let path = root.join(EVENT_LOG_RELATIVE_PATH);
    let Some(bytes) = read_regular_private(&path).map_err(|err| {
        RunAssetError::new(format!(
            "read public journal {} failed: {err}",
            path.display()
        ))
    })?
    else {
        return Ok(ValidatedJournalState::empty(disk_fingerprint(root)?));
    };
    if mode == JournalReadMode::RecoverTornTail && !bytes.is_empty() && !bytes.ends_with(b"\n") {
        let prefix_end = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let (committed, torn_tail) = bytes.split_at(prefix_end);
        let events = parse_events(&path, committed)?;
        quarantine_torn_tail(root, torn_tail)?;
        truncate_private(&path, committed.len() as u64).map_err(|err| {
            RunAssetError::new(format!(
                "recover public journal {} failed: {err}",
                path.display()
            ))
        })?;
        return Ok(ValidatedJournalState::from_events(
            disk_fingerprint(root)?,
            committed,
            events,
        ));
    }
    let events = parse_events(&path, &bytes)?;
    Ok(ValidatedJournalState::from_events(
        disk_fingerprint(root)?,
        &bytes,
        events,
    ))
}

fn disk_fingerprint(root: &Path) -> Result<JournalDiskFingerprint, RunAssetError> {
    let path = root.join(EVENT_LOG_RELATIVE_PATH);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => Ok(JournalDiskFingerprint {
            byte_len: metadata.len(),
            modified_at: metadata.modified().ok(),
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(JournalDiskFingerprint {
            byte_len: 0,
            modified_at: None,
        }),
        Err(err) => Err(RunAssetError::new(format!(
            "inspect public journal {} failed: {err}",
            path.display()
        ))),
    }
}

fn parse_events(path: &Path, bytes: &[u8]) -> Result<Vec<PublicJournalEvent>, RunAssetError> {
    let mut events = Vec::new();
    let mut ids = BTreeSet::new();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let event = serde_json::from_slice::<PublicJournalEvent>(line).map_err(|err| {
            RunAssetError::new(format!(
                "parse public journal {} line {} failed: {err}",
                path.display(),
                index + 1
            ))
        })?;
        if event.schema_name != EVENT_SCHEMA_NAME || event.schema_major != JOURNAL_SCHEMA_MAJOR {
            return Err(RunAssetError::new(format!(
                "public journal {} line {} has unsupported schema",
                path.display(),
                index + 1
            )));
        }
        validate_event_payload(path, index + 1, &event)?;
        if event.seq != events.len() as u64 + 1 {
            return Err(RunAssetError::new(format!(
                "public journal {} line {} is not contiguous",
                path.display(),
                index + 1
            )));
        }
        if !ids.insert(event.event_id.clone()) {
            return Err(RunAssetError::new(format!(
                "public journal {} line {} repeats an event id",
                path.display(),
                index + 1
            )));
        }
        events.push(event);
    }
    Ok(events)
}

fn validate_event_payload(
    path: &Path,
    line_number: usize,
    event: &PublicJournalEvent,
) -> Result<(), RunAssetError> {
    let Some(kind) = PublicEventKind::parse(&event.kind) else {
        return Ok(());
    };
    validate_known_wire_payload(kind, &event.data).map_err(|err| {
        RunAssetError::new(format!(
            "public journal {} line {} has malformed {} payload: {err}",
            path.display(),
            line_number,
            kind.as_str()
        ))
    })
}

fn recover_torn_tail(
    root: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<PublicJournalEvent>, RunAssetError> {
    let prefix_end = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let (committed, torn_tail) = bytes.split_at(prefix_end);
    let events = parse_events(path, committed)?;
    quarantine_torn_tail(root, torn_tail)?;
    truncate_private(path, committed.len() as u64).map_err(|err| {
        RunAssetError::new(format!(
            "recover public journal {} failed: {err}",
            path.display()
        ))
    })?;
    Ok(events)
}

fn quarantine_torn_tail(root: &Path, torn_tail: &[u8]) -> Result<(), RunAssetError> {
    if torn_tail.is_empty() {
        return Ok(());
    }
    let quarantine_path = root.join("records/quarantine").join(format!(
        "events-torn-tail-{:016x}-{}.fragment",
        stable_tail_hash(torn_tail),
        torn_tail.len()
    ));
    atomic_write_private(&quarantine_path, torn_tail).map_err(|err| {
        RunAssetError::new(format!(
            "quarantine public journal torn tail {} failed: {err}",
            quarantine_path.display()
        ))
    })
}

fn quarantine_invalid_seal(manifest: &RunAssetManifest, bytes: &[u8]) -> Result<(), RunAssetError> {
    let hash = format!("{:x}", Sha256::digest(bytes));
    let quarantine_path = manifest
        .root
        .join("records/quarantine/invalid-seals")
        .join(format!("journal-seal-{hash}.json"));
    if let Some(parent) = quarantine_path.parent() {
        create_dir_all(parent)?;
        ensure_private_dir(parent)?;
    }
    if read_regular_private(&quarantine_path)?.is_none() {
        write_create_new_private(&quarantine_path, bytes)?;
    }
    let seal_path = manifest.root.join(SEAL_RELATIVE_PATH);
    let metadata = fs::symlink_metadata(&seal_path).map_err(|err| {
        RunAssetError::new(format!(
            "inspect invalid public journal seal {} failed: {err}",
            seal_path.display()
        ))
    })?;
    let removal = if metadata.file_type().is_dir() {
        fs::remove_dir_all(&seal_path)
    } else {
        fs::remove_file(&seal_path)
    };
    removal.map_err(|err| {
        RunAssetError::new(format!(
            "remove invalid public journal seal {} failed: {err}",
            seal_path.display()
        ))
    })?;
    if let Some(parent) = seal_path.parent() {
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|err| {
                RunAssetError::new(format!(
                    "sync public journal directory {} failed: {err}",
                    parent.display()
                ))
            })?;
    }
    Ok(())
}

fn has_journal_quarantine(root: &Path) -> Result<bool, RunAssetError> {
    let path = root.join("records/quarantine");
    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(RunAssetError::new(format!(
                "read journal quarantine {} failed: {err}",
                path.display()
            )));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|err| {
            RunAssetError::new(format!(
                "read journal quarantine {} failed: {err}",
                path.display()
            ))
        })?;
        if entry.file_name() != "invalid-seals" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_seal(root: &Path) -> Result<Option<JournalSeal>, RunAssetError> {
    let path = root.join(SEAL_RELATIVE_PATH);
    let bytes = match inspect_public_seal(&path)? {
        PublicSealEntry::Missing => return Ok(None),
        PublicSealEntry::Bytes(bytes) => bytes,
        PublicSealEntry::Unsafe(kind) => {
            return Err(RunAssetError::new(format!(
                "journal seal is an unsafe public {kind}"
            )));
        }
    };
    let seal = serde_json::from_slice::<JournalSeal>(&bytes)
        .map_err(|err| RunAssetError::new(format!("parse journal seal failed: {err}")))?;
    if seal.schema_name != JOURNAL_SCHEMA_NAME || seal.schema_major != JOURNAL_SCHEMA_MAJOR {
        return Err(RunAssetError::new("journal seal has unsupported schema"));
    }
    Ok(Some(seal))
}

enum PublicSealEntry {
    Missing,
    Bytes(Vec<u8>),
    Unsafe(String),
}

fn inspect_public_seal(path: &Path) -> Result<PublicSealEntry, RunAssetError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PublicSealEntry::Missing);
        }
        Err(err) => {
            return Err(RunAssetError::new(format!(
                "inspect journal seal {} failed: {err}",
                path.display()
            )));
        }
    };
    if !metadata.file_type().is_file() {
        return Ok(PublicSealEntry::Unsafe("non-regular entry".to_string()));
    }
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
        return Ok(PublicSealEntry::Unsafe(
            "foreign or multiply linked file".to_string(),
        ));
    }
    fs::read(path).map(PublicSealEntry::Bytes).map_err(|err| {
        RunAssetError::new(format!(
            "read journal seal {} failed: {err}",
            path.display()
        ))
    })
}

fn stable_event_id(salt: &[u8], run_id: &str, kind: &str, source_ref: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(b"\0event\0");
    hasher.update(run_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(source_ref.as_bytes());
    format!("evt-sha256:{:x}", hasher.finalize())
}

fn stable_public_ref(salt: &[u8], namespace: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(b"\0");
    hasher.update(namespace.as_bytes());
    hasher.update(b"\0");
    hasher.update(value.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn public_ref_salt() -> Result<Vec<u8>, RunAssetError> {
    let state_root = crate::state_path::user_state_root().map_err(|err| {
        RunAssetError::new(format!("resolve public reference salt root failed: {err}"))
    })?;
    let path = state_root.join("public-ref-salt");
    if let Some(salt) = PUBLIC_REF_SALTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| RunAssetError::new("public reference salt cache lock is poisoned"))?
        .get(&path)
        .cloned()
    {
        return Ok(salt);
    }
    create_dir_all(&state_root)?;
    ensure_private_dir(&state_root)?;
    let salt = match read_regular_private(&path)? {
        Some(salt) => validate_public_ref_salt(&salt)?,
        None => {
            let mut salt = vec![0_u8; 32];
            fs::File::open("/dev/urandom")
                .and_then(|mut file| file.read_exact(&mut salt))
                .map_err(|err| {
                    RunAssetError::new(format!("generate public reference salt failed: {err}"))
                })?;
            if let Err(write_error) = write_create_new_private(&path, &salt) {
                let Some(existing) = read_regular_private(&path)? else {
                    return Err(write_error);
                };
                salt = validate_public_ref_salt(&existing)?;
            }
            salt
        }
    };
    PUBLIC_REF_SALTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| RunAssetError::new("public reference salt cache lock is poisoned"))?
        .insert(path, salt.clone());
    Ok(salt)
}

fn validate_public_ref_salt(salt: &[u8]) -> Result<Vec<u8>, RunAssetError> {
    if salt.len() != 32 {
        return Err(RunAssetError::new(
            "public reference salt must contain exactly 32 bytes",
        ));
    }
    Ok(salt.to_vec())
}

fn event_semantic_sha256(event: &PublicJournalEvent) -> String {
    let value = json!({
        "schema_name": event.schema_name,
        "schema_major": event.schema_major,
        "event_id": event.event_id,
        "run_ref": event.run_ref,
        "activation_ref": event.activation_ref,
        "session_ref": event.session_ref,
        "revision_ref": event.revision_ref,
        "kind": event.kind,
        "caused_by_seq": event.caused_by_seq,
        "correlation_ref": event.correlation_ref,
        "data": event.data,
        "extra": event.extra,
    });
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn event_context_generation(event: &PublicJournalEvent) -> Option<u64> {
    event
        .data
        .pointer("/payload/context_generation")
        .and_then(Value::as_u64)
}

fn stable_tail_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::MutexGuard;

    use super::super::public_event::*;
    use super::super::{RunAssetSink, RunAssetStore};
    use super::*;

    static TEST_TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);
    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn test_lock() -> MutexGuard<'static, ()> {
        TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn temp_root(name: &str) -> PathBuf {
        let index = TEST_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("temp")
            .join(format!("journal-{name}-{}-{index}", std::process::id()));
        if path.exists() {
            fs::remove_dir_all(&path).unwrap();
        }
        path
    }

    fn reset_writer_state() {
        if let Some(states) = JOURNAL_WRITER_STATES.get() {
            states
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clear();
        }
        JOURNAL_FULL_SCANS.store(0, Ordering::Relaxed);
        if let Some(scans) = JOURNAL_FULL_SCANS_BY_ROOT.get() {
            scans
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clear();
        }
    }

    fn scan_count(root: &Path) -> usize {
        JOURNAL_FULL_SCANS_BY_ROOT
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(root)
            .copied()
            .unwrap_or(0)
    }

    fn content(with_path: bool) -> PublicContentRef {
        PublicContentRef {
            sha256: format!("sha256:{}", "a".repeat(64)),
            content_ref: format!("sha256:{}", "a".repeat(64)),
            length: 1,
            path: with_path.then(|| "content/facts/a.bin".to_string()),
        }
    }

    fn minimal_payload(kind: PublicEventKind) -> PublicEventPayload {
        match kind {
            PublicEventKind::RunStarted => PublicEventPayload::Run(RunLifecyclePayload {
                transition: RunTransition::Started,
                mode: Some("finite".to_string()),
                status: Some("running".to_string()),
                reason: None,
                activation_limit: Some(1),
                stop_attempt_limit: Some(1),
            }),
            PublicEventKind::RunStatus => PublicEventPayload::Run(RunLifecyclePayload {
                transition: RunTransition::Status,
                mode: None,
                status: Some("paused".to_string()),
                reason: None,
                activation_limit: None,
                stop_attempt_limit: None,
            }),
            PublicEventKind::RunCompleted => PublicEventPayload::Run(RunLifecyclePayload {
                transition: RunTransition::Completed,
                mode: None,
                status: Some("completed".to_string()),
                reason: None,
                activation_limit: None,
                stop_attempt_limit: None,
            }),
            PublicEventKind::FlowRevisionPrepared
            | PublicEventKind::FlowRevisionApplied
            | PublicEventKind::FlowRevisionRejected => {
                let state = match kind {
                    PublicEventKind::FlowRevisionPrepared => RevisionState::Prepared,
                    PublicEventKind::FlowRevisionApplied => RevisionState::Applied,
                    PublicEventKind::FlowRevisionRejected => RevisionState::Rejected,
                    _ => unreachable!(),
                };
                PublicEventPayload::Revision(RevisionLifecyclePayload {
                    state,
                    flow_id: "flow-a".to_string(),
                    content: content(true),
                    review_status: Some("approved".to_string()),
                })
            }
            PublicEventKind::FactRecorded => PublicEventPayload::Fact(FactPayload {
                fact_kind: FactKind::Explicit,
                key: "fact-a".to_string(),
                version: Some(1),
                content: content(true),
            }),
            PublicEventKind::ArtifactRecorded => PublicEventPayload::Artifact(ArtifactPayload {
                artifact_id: "artifact-a".to_string(),
                artifact_key: "brief".to_string(),
                content: content(true),
            }),
            PublicEventKind::RouteDecided => PublicEventPayload::Route(RoutePayload {
                flow_id: "flow-a".to_string(),
                route_id: "route-a".to_string(),
                route_index: 0,
                predicate: "exists(artifact.brief)".to_string(),
                for_each: None,
                source_artifact_id: None,
                trigger_fact: "artifact.brief".to_string(),
                trigger_version: 1,
                planned_activation_ids: vec!["follow".to_string()],
                applied_activation_ids: vec!["follow".to_string()],
            }),
            PublicEventKind::ActivationCreated
            | PublicEventKind::ActivationStatus
            | PublicEventKind::ActivationCompleted => {
                let state = match kind {
                    PublicEventKind::ActivationCreated => ActivationState::Planned,
                    PublicEventKind::ActivationStatus => ActivationState::Running,
                    PublicEventKind::ActivationCompleted => ActivationState::Completed,
                    _ => unreachable!(),
                };
                PublicEventPayload::Activation(ActivationLifecyclePayload {
                    state,
                    node_id: Some("root".to_string()),
                    allocation_generation: Some(0),
                    termination: None,
                })
            }
            PublicEventKind::AgentSessionStarted
            | PublicEventKind::AgentSessionBound
            | PublicEventKind::AgentSessionEnded => {
                let state = match kind {
                    PublicEventKind::AgentSessionStarted => SessionState::Started,
                    PublicEventKind::AgentSessionBound => SessionState::Bound,
                    PublicEventKind::AgentSessionEnded => SessionState::Ended,
                    _ => unreachable!(),
                };
                PublicEventPayload::Session(SessionLifecyclePayload {
                    state,
                    platform: NativePlatform::Codex,
                    exit_status: None,
                })
            }
            PublicEventKind::HookObserved => PublicEventPayload::Hook(HookPayload {
                hook_kind: HookKind::AgentReady,
                detail: content(false),
                status: Some("ready".to_string()),
                allocation_generation: Some(0),
                context_generation: Some(0),
            }),
            PublicEventKind::ContextCompactionStarted
            | PublicEventKind::ContextCompactionFinished => {
                PublicEventPayload::Compaction(CompactionPayload {
                    state: if kind == PublicEventKind::ContextCompactionStarted {
                        CompactionState::Started
                    } else {
                        CompactionState::Finished
                    },
                    context_generation: 0,
                })
            }
            PublicEventKind::WorkProfileObserved => {
                PublicEventPayload::WorkProfile(WorkProfilePayload {
                    intent: "produce".to_string(),
                    workspace_access: "read_write".to_string(),
                    tool_execution: "allowed".to_string(),
                    network_access: "restricted".to_string(),
                })
            }
            PublicEventKind::QosObserved | PublicEventKind::QosApplied => {
                PublicEventPayload::Qos(QosPayload {
                    state: if kind == PublicEventKind::QosObserved {
                        QosState::Observed
                    } else {
                        QosState::Applied
                    },
                    urgency: "interactive".to_string(),
                    completion_target: None,
                })
            }
            PublicEventKind::UsageObserved => PublicEventPayload::Usage(UsagePayload {
                metric: UsageMetric::TotalTokens,
                value: 1,
            }),
            PublicEventKind::MachineInputDelivered => {
                PublicEventPayload::MachineInput(MachineInputPayload {
                    status: "submitted".to_string(),
                    content: content(false),
                    submit_key_count: 1,
                })
            }
            PublicEventKind::StopObserved | PublicEventKind::StopDecided => {
                PublicEventPayload::Stop(StopPayload {
                    stage: if kind == PublicEventKind::StopObserved {
                        StopStage::Observed
                    } else {
                        StopStage::Decided
                    },
                    decision: None,
                    attempt: None,
                    missing_artifacts: Vec::new(),
                    missing_effects: Vec::new(),
                    reason: None,
                })
            }
        }
    }

    fn minimal_data(kind: PublicEventKind) -> Value {
        let payload = minimal_payload(kind);
        let public_ref = |namespace: &str, value: &str| format!("{namespace}:{value}");
        wire_data(
            PublicSource::Runtime,
            "source:1".to_string(),
            &payload,
            &PublicRefMapper::new(&public_ref),
        )
    }

    fn event_line(kind: &str, data: Value, schema_major: u32) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&json!({
            "schema_name": EVENT_SCHEMA_NAME,
            "schema_major": schema_major,
            "seq": 1,
            "event_id": format!("event:{kind}"),
            "occurred_at_ms": 1,
            "run_ref": "run:test",
            "kind": kind,
            "data": data,
            "future_root": true,
        }))
        .unwrap();
        bytes.push(b'\n');
        bytes
    }

    fn route_input(run_id: &str, index: usize) -> PublicJournalInput {
        PublicJournalInput {
            kind: PublicEventKind::RouteDecided,
            run_id: run_id.to_string(),
            activation_id: None,
            session_id: None,
            revision_id: None,
            caused_by_seq: None,
            correlation_id: None,
            source: PublicSource::Runtime,
            source_native_id: format!("route:{index}"),
            payload: PublicEventPayload::Route(RoutePayload {
                flow_id: "flow-a".to_string(),
                route_id: format!("route-{index}"),
                route_index: index as u64,
                predicate: "exists(artifact.brief)".to_string(),
                for_each: None,
                source_artifact_id: None,
                trigger_fact: "artifact.brief".to_string(),
                trigger_version: index as u64,
                planned_activation_ids: vec![format!("activation-{index}")],
                applied_activation_ids: vec![format!("activation-{index}")],
            }),
        }
    }

    fn append_unknown_event(path: &Path, seq: u64) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        let mut line = serde_json::to_vec(&json!({
            "schema_name": EVENT_SCHEMA_NAME,
            "schema_major": JOURNAL_SCHEMA_MAJOR,
            "seq": seq,
            "event_id": format!("event:future:{seq}"),
            "occurred_at_ms": seq,
            "run_ref": "run:linear",
            "kind": "future.kind",
            "data": {"opaque": true},
        }))
        .unwrap();
        line.push(b'\n');
        file.write_all(&line).unwrap();
        file.sync_all().unwrap();
    }

    #[test]
    fn public_event_kind_model_is_closed_and_external_strings_are_stable() {
        let mut seen = BTreeSet::new();
        for kind in PublicEventKind::ALL {
            let wire = kind.as_str();
            assert!(seen.insert(wire), "duplicate wire kind {wire}");
            assert_eq!(PublicEventKind::parse(wire), Some(*kind));
            validate_known_wire_payload(*kind, &minimal_data(*kind)).unwrap();
        }
        assert!(seen.contains("activation.completed"));
        assert!(seen.contains("context_compaction.finished"));
        assert_eq!(seen.len(), PublicEventKind::ALL.len());
    }

    #[test]
    fn known_payload_validation_rejects_malformed_data_and_allows_additive_fields() {
        let malformed = event_line(
            PublicEventKind::MachineInputDelivered.as_str(),
            json!({
                "source": "runtime",
                "source_ref": "source:1",
                "payload": {"other": true}
            }),
            JOURNAL_SCHEMA_MAJOR,
        );
        let err = parse_events(Path::new("events.jsonl"), &malformed).unwrap_err();
        assert!(
            err.to_string()
                .contains("malformed machine_input.delivered")
        );

        let mut additive_data = minimal_data(PublicEventKind::MachineInputDelivered);
        additive_data["future_data"] = json!({"preserved": true});
        let parsed = parse_events(
            Path::new("events.jsonl"),
            &event_line(
                PublicEventKind::MachineInputDelivered.as_str(),
                additive_data,
                JOURNAL_SCHEMA_MAJOR,
            ),
        )
        .unwrap();
        assert_eq!(parsed[0].data["future_data"]["preserved"], true);
        assert_eq!(parsed[0].extra["future_root"], true);
    }

    #[test]
    fn unknown_same_major_events_are_opaque_and_unknown_major_is_rejected() {
        let unknown = parse_events(
            Path::new("events.jsonl"),
            &event_line(
                "future.kind",
                json!({"future": {"shape": true}}),
                JOURNAL_SCHEMA_MAJOR,
            ),
        )
        .unwrap();
        assert_eq!(unknown[0].kind, "future.kind");
        assert_eq!(unknown[0].data["future"]["shape"], true);
        assert_eq!(unknown[0].extra["future_root"], true);

        let err = parse_events(
            Path::new("events.jsonl"),
            &event_line(
                "future.kind",
                json!({"future": true}),
                JOURNAL_SCHEMA_MAJOR + 1,
            ),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unsupported schema"));
    }

    #[test]
    fn high_volume_append_and_summary_reuse_validated_state() {
        let _guard = test_lock();
        let root = temp_root("linear");
        let store =
            RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_000);
        let manifest = store.start_run_manifest("run-linear").unwrap();
        reset_writer_state();

        for index in 0..2_000 {
            append_event(
                &manifest,
                route_input(&manifest.run_id, index),
                1_700_000_000_001,
            )
            .unwrap();
            let summary = manifest_summary(&manifest).unwrap();
            assert_eq!(summary["last_seq"], (index + 1) as u64);
        }

        let scan_count = scan_count(&manifest.root);
        assert!(
            scan_count <= 2,
            "append path scanned {scan_count} times for 2,000 appends"
        );
    }

    #[test]
    fn missing_stale_and_tampered_writer_state_rebuilds_from_public_journal() {
        let _guard = test_lock();
        let root = temp_root("cursor-rebuild");
        let store =
            RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_000);
        let manifest = store.start_run_manifest("run-linear").unwrap();
        reset_writer_state();

        append_event(
            &manifest,
            route_input(&manifest.run_id, 0),
            1_700_000_000_001,
        )
        .unwrap();
        assert_eq!(scan_count(&manifest.root), 1);

        reset_writer_state();
        append_event(
            &manifest,
            route_input(&manifest.run_id, 1),
            1_700_000_000_002,
        )
        .unwrap();
        assert_eq!(scan_count(&manifest.root), 1);

        let events_path = manifest.root.join(EVENT_LOG_RELATIVE_PATH);
        append_unknown_event(&events_path, 3);
        append_event(
            &manifest,
            route_input(&manifest.run_id, 2),
            1_700_000_000_003,
        )
        .unwrap();
        assert_eq!(scan_count(&manifest.root), 2);

        {
            let mut states = journal_writer_states()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let state = states.get_mut(&manifest.root).unwrap();
            state.fingerprint.byte_len = state.fingerprint.byte_len.saturating_add(1);
        }
        append_event(
            &manifest,
            route_input(&manifest.run_id, 3),
            1_700_000_000_004,
        )
        .unwrap();
        assert_eq!(scan_count(&manifest.root), 3);
    }

    #[test]
    fn crash_tail_recovery_rebuilds_state_and_preserves_torn_fragment() {
        let _guard = test_lock();
        let root = temp_root("crash-tail");
        let store =
            RunAssetStore::new_with_fixed_clock(RunAssetSink::Root(root), 1_700_000_000_000);
        let manifest = store.start_run_manifest("run-linear").unwrap();
        reset_writer_state();

        append_event(
            &manifest,
            route_input(&manifest.run_id, 0),
            1_700_000_000_001,
        )
        .unwrap();
        let events_path = manifest.root.join(EVENT_LOG_RELATIVE_PATH);
        fs::OpenOptions::new()
            .append(true)
            .open(&events_path)
            .unwrap()
            .write_all(br#"{"seq":999"#)
            .unwrap();

        append_event(
            &manifest,
            route_input(&manifest.run_id, 1),
            1_700_000_000_002,
        )
        .unwrap();
        assert!(manifest.root.join("records/quarantine").exists());
        let events = read_events(&manifest, JournalReadMode::Strict).unwrap();
        assert_eq!(events.last().unwrap().seq, 2);
        assert_eq!(manifest_summary(&manifest).unwrap()["status"], "corrupt");
    }
}
