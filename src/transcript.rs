use std::collections::BTreeSet;

use crate::input_ledger::{
    MachineInputRecord, MachineInputStatus, machine_input_payload_hash,
    normalize_machine_input_text,
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TranscriptMessage {
    pub role: TranscriptRole,
    pub text: String,
    pub timestamp_ms: Option<u64>,
    pub transaction_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TranscriptRole {
    User,
    Assistant,
    System,
    Other(String),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TranscriptClassification {
    pub classification: TranscriptInputClassification,
    pub transaction_id: Option<String>,
}

impl TranscriptClassification {
    fn new(classification: TranscriptInputClassification, transaction_id: Option<String>) -> Self {
        Self {
            classification,
            transaction_id,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TranscriptInputClassification {
    HumanizeInjected,
    LikelyHumanizeInjected,
    InferredHumanIntervention,
    Unclassified,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TranscriptClassifierConfig {
    pub close_timestamp_window_ms: u64,
}

impl Default for TranscriptClassifierConfig {
    fn default() -> Self {
        Self {
            close_timestamp_window_ms: 5_000,
        }
    }
}

pub fn classify_transcript_message(
    message: &TranscriptMessage,
    records: &[MachineInputRecord],
    config: &TranscriptClassifierConfig,
) -> TranscriptClassification {
    classify_transcript_messages(std::slice::from_ref(message), records, config)
        .into_iter()
        .next()
        .expect("single message classification should return one result")
}

pub fn classify_transcript_messages(
    messages: &[TranscriptMessage],
    records: &[MachineInputRecord],
    config: &TranscriptClassifierConfig,
) -> Vec<TranscriptClassification> {
    let mut used_transactions = BTreeSet::new();
    messages
        .iter()
        .map(|message| classify_next_message(message, records, config, &mut used_transactions))
        .collect()
}

fn classify_next_message(
    message: &TranscriptMessage,
    records: &[MachineInputRecord],
    config: &TranscriptClassifierConfig,
    used_transactions: &mut BTreeSet<String>,
) -> TranscriptClassification {
    if message.role != TranscriptRole::User {
        return TranscriptClassification::new(TranscriptInputClassification::Unclassified, None);
    }

    let normalized_text = normalize_machine_input_text(&message.text);
    if normalized_text.is_empty() {
        return TranscriptClassification::new(TranscriptInputClassification::Unclassified, None);
    }
    let payload_hash = machine_input_payload_hash(&normalized_text);

    if let Some(transaction_id) = message.transaction_id.as_deref() {
        if let Some(record) = best_record(records.iter().filter(|record| {
            record.status == MachineInputStatus::Submitted
                && !used_transactions.contains(&record.transaction_id)
                && record.transaction_id == transaction_id
                && record_matches_payload(record, &normalized_text, &payload_hash)
        })) {
            used_transactions.insert(record.transaction_id.clone());
            return TranscriptClassification::new(
                TranscriptInputClassification::HumanizeInjected,
                Some(record.transaction_id.clone()),
            );
        }
    }

    if let Some(timestamp_ms) = message.timestamp_ms {
        if let Some(record) = best_time_record(
            records.iter().filter(|record| {
                record.status == MachineInputStatus::Submitted
                    && !used_transactions.contains(&record.transaction_id)
                    && record_matches_payload(record, &normalized_text, &payload_hash)
                    && timestamp_is_close(
                        timestamp_ms,
                        record.submitted_at_ms,
                        config.close_timestamp_window_ms,
                    )
            }),
            timestamp_ms,
        ) {
            used_transactions.insert(record.transaction_id.clone());
            return TranscriptClassification::new(
                TranscriptInputClassification::LikelyHumanizeInjected,
                Some(record.transaction_id.clone()),
            );
        }
    }

    TranscriptClassification::new(
        TranscriptInputClassification::InferredHumanIntervention,
        None,
    )
}

fn record_matches_payload(record: &MachineInputRecord, normalized_text: &str, hash: &str) -> bool {
    record.normalized_text == normalized_text && record.payload_hash == hash
}

fn best_record<'a>(
    records: impl Iterator<Item = &'a MachineInputRecord>,
) -> Option<&'a MachineInputRecord> {
    records.min_by_key(|record| status_priority(&record.status))
}

fn best_time_record<'a>(
    records: impl Iterator<Item = &'a MachineInputRecord>,
    timestamp_ms: u64,
) -> Option<&'a MachineInputRecord> {
    records.min_by_key(|record| {
        (
            status_priority(&record.status),
            timestamp_ms.abs_diff(record.submitted_at_ms),
        )
    })
}

fn status_priority(status: &MachineInputStatus) -> u8 {
    match status {
        MachineInputStatus::Submitted => 0,
        MachineInputStatus::Started => 1,
        MachineInputStatus::Failed => 2,
    }
}

fn timestamp_is_close(left: u64, right: u64, window_ms: u64) -> bool {
    left.abs_diff(right) <= window_ms
}
