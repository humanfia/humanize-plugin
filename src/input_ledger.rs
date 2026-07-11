use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::run_assets::{RunAssetStore, append_machine_input_ledger_direct};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineInputRecord {
    pub run_id: String,
    pub activation_id: String,
    pub pane_id: String,
    pub started_at_ms: u64,
    pub submitted_at_ms: u64,
    pub payload_hash: String,
    pub normalized_text: String,
    pub submit_key_count: usize,
    pub transaction_id: String,
    pub status: MachineInputStatus,
}

impl MachineInputRecord {
    pub fn started(submission: MachineInputSubmission<'_>) -> Self {
        Self::from_submission(submission, MachineInputStatus::Started)
    }

    pub fn submitted(submission: MachineInputSubmission<'_>) -> Self {
        Self::from_submission(submission, MachineInputStatus::Submitted)
    }

    pub fn failed(submission: MachineInputSubmission<'_>) -> Self {
        Self::from_submission(submission, MachineInputStatus::Failed)
    }

    fn from_submission(submission: MachineInputSubmission<'_>, status: MachineInputStatus) -> Self {
        let normalized_text = normalize_machine_input_text(submission.text);
        let payload_hash = machine_input_payload_hash(&normalized_text);
        Self {
            run_id: submission.run_id.to_string(),
            activation_id: submission.activation_id.to_string(),
            pane_id: submission.pane_id.to_string(),
            started_at_ms: submission.started_at_ms,
            submitted_at_ms: submission.submitted_at_ms,
            payload_hash,
            normalized_text,
            submit_key_count: submission.submit_key_count,
            transaction_id: submission.transaction_id,
            status,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MachineInputSubmission<'a> {
    pub run_id: &'a str,
    pub activation_id: &'a str,
    pub pane_id: &'a str,
    pub started_at_ms: u64,
    pub submitted_at_ms: u64,
    pub text: &'a str,
    pub submit_key_count: usize,
    pub transaction_id: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineInputStatus {
    Started,
    Submitted,
    Failed,
}

#[derive(Debug, Clone)]
pub struct MachineInputLedger {
    sink: MachineInputLedgerSink,
    sequence: Arc<AtomicU64>,
}

impl MachineInputLedger {
    pub fn runtime_default() -> Self {
        Self::new(MachineInputLedgerSink::RuntimeDefault)
    }

    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        Self::new(MachineInputLedgerSink::JsonlPath(path.into()))
    }

    pub fn in_memory() -> Self {
        Self::new(MachineInputLedgerSink::Memory(Arc::new(Mutex::new(
            Vec::new(),
        ))))
    }

    pub fn runtime_default_path(run_id: &str) -> PathBuf {
        RunAssetStore::runtime_default()
            .run_root(run_id)
            .unwrap_or_else(|_| PathBuf::from(".").join(machine_input_run_path_segment(run_id)))
            .join("machine-inputs.jsonl")
    }

    pub fn append(&self, record: MachineInputRecord) -> Result<(), MachineInputLedgerError> {
        match &self.sink {
            MachineInputLedgerSink::RuntimeDefault => {
                append_jsonl(&Self::runtime_default_path(&record.run_id), &record)
            }
            MachineInputLedgerSink::JsonlPath(path) => append_jsonl(path, &record),
            MachineInputLedgerSink::Memory(records) => {
                let mut records = records.lock().map_err(|_| {
                    MachineInputLedgerError::new("machine input ledger memory lock failed")
                })?;
                records.push(record);
                Ok(())
            }
        }
    }

    pub fn records(&self) -> Vec<MachineInputRecord> {
        match &self.sink {
            MachineInputLedgerSink::Memory(records) => records
                .lock()
                .map(|records| records.clone())
                .unwrap_or_default(),
            MachineInputLedgerSink::RuntimeDefault | MachineInputLedgerSink::JsonlPath(_) => {
                Vec::new()
            }
        }
    }

    pub fn jsonl_lines(&self) -> Vec<String> {
        self.records()
            .into_iter()
            .filter_map(|record| serde_json::to_string(&record).ok())
            .collect()
    }

    pub fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn new(sink: MachineInputLedgerSink) -> Self {
        Self {
            sink,
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }
}

#[derive(Debug, Clone)]
enum MachineInputLedgerSink {
    RuntimeDefault,
    JsonlPath(PathBuf),
    Memory(Arc<Mutex<Vec<MachineInputRecord>>>),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MachineInputLedgerError {
    message: String,
}

impl MachineInputLedgerError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for MachineInputLedgerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl Error for MachineInputLedgerError {}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MachineInputClock {
    Realtime,
    Fixed(u64),
}

impl MachineInputClock {
    pub fn realtime() -> Self {
        Self::Realtime
    }

    pub fn fixed(timestamp_ms: u64) -> Self {
        Self::Fixed(timestamp_ms)
    }

    pub fn now_ms(&self) -> u64 {
        match self {
            Self::Realtime => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0),
            Self::Fixed(timestamp_ms) => *timestamp_ms,
        }
    }
}

pub fn normalize_machine_input_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_end_matches('\n')
        .to_string()
}

pub fn machine_input_payload_hash(text: &str) -> String {
    let normalized = normalize_machine_input_text(text);
    format!("fnv1a64:{:016x}", stable_hash(normalized.as_bytes()))
}

pub fn machine_input_transaction_id(
    run_id: &str,
    activation_id: &str,
    pane_id: &str,
    payload_hash: &str,
    started_at_ms: u64,
    sequence: u64,
) -> String {
    let payload = format!(
        "{run_id}\0{activation_id}\0{pane_id}\0{payload_hash}\0{started_at_ms}\0{sequence}"
    );
    format!("machine-input:{:016x}", stable_hash(payload.as_bytes()))
}

pub fn machine_input_run_path_segment(run_id: &str) -> String {
    if is_safe_run_path_segment(run_id) {
        return run_id.to_string();
    }

    let candidate: String = run_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let candidate = candidate.trim_matches('.');
    let base = if candidate.is_empty() || candidate == "." || candidate == ".." {
        "run"
    } else {
        candidate
    };
    format!("{base}-{:016x}", stable_hash(run_id.as_bytes()))
}

fn is_safe_run_path_segment(run_id: &str) -> bool {
    !run_id.is_empty()
        && run_id != "."
        && run_id != ".."
        && run_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

fn append_jsonl(path: &Path, record: &MachineInputRecord) -> Result<(), MachineInputLedgerError> {
    append_machine_input_ledger_direct(path, record).map_err(|err| {
        MachineInputLedgerError::new(format!(
            "write machine input ledger {} failed: {err}",
            path.display()
        ))
    })
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
