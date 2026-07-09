use serde_json::Value;

use humanize_plugin::input_ledger::{
    MachineInputLedger, MachineInputRecord, MachineInputStatus, machine_input_payload_hash,
    normalize_machine_input_text,
};
use humanize_plugin::transcript::{
    TranscriptClassification, TranscriptClassifierConfig, TranscriptInputClassification,
    TranscriptMessage, TranscriptRole, classify_transcript_message, classify_transcript_messages,
};

fn record(transaction_id: &str, text: &str, submitted_at_ms: u64) -> MachineInputRecord {
    record_with_status(
        transaction_id,
        text,
        submitted_at_ms,
        MachineInputStatus::Submitted,
    )
}

fn record_with_status(
    transaction_id: &str,
    text: &str,
    submitted_at_ms: u64,
    status: MachineInputStatus,
) -> MachineInputRecord {
    let normalized_text = normalize_machine_input_text(text);
    MachineInputRecord {
        run_id: "run-a".to_string(),
        activation_id: "activation-a".to_string(),
        pane_id: "%8".to_string(),
        started_at_ms: submitted_at_ms.saturating_sub(10),
        submitted_at_ms,
        payload_hash: machine_input_payload_hash(&normalized_text),
        normalized_text,
        submit_key_count: 1,
        transaction_id: transaction_id.to_string(),
        status,
    }
}

#[test]
fn machine_input_ledger_keeps_records_in_memory_and_renders_jsonl() {
    let ledger = MachineInputLedger::in_memory();
    let input_record = record("tx-a", "inspect\r\nthe repo", 1_000);

    ledger.append(input_record.clone()).unwrap();

    assert_eq!(ledger.records(), vec![input_record.clone()]);
    let jsonl = ledger.jsonl_lines().join("\n");
    let parsed: Value = serde_json::from_str(&jsonl).unwrap();
    assert_eq!(parsed["run_id"], "run-a");
    assert_eq!(parsed["activation_id"], "activation-a");
    assert_eq!(parsed["pane_id"], "%8");
    assert_eq!(parsed["started_at_ms"], 990);
    assert_eq!(parsed["submitted_at_ms"], 1_000);
    assert_eq!(parsed["normalized_text"], "inspect\nthe repo");
    assert_eq!(
        parsed["payload_hash"],
        machine_input_payload_hash("inspect\nthe repo")
    );
    assert_eq!(parsed["submit_key_count"], 1);
    assert_eq!(parsed["transaction_id"], "tx-a");
    assert_eq!(parsed["status"], "submitted");
}

#[test]
fn machine_input_ledger_runtime_default_path_is_under_cache_runs() {
    let path = MachineInputLedger::runtime_default_path("run-a");

    assert_eq!(path.file_name().unwrap(), "machine-inputs.jsonl");
    assert_eq!(path.parent().unwrap().file_name().unwrap(), "run-a");
    assert_eq!(
        path.parent()
            .unwrap()
            .parent()
            .unwrap()
            .file_name()
            .unwrap(),
        "runs"
    );
    assert_eq!(
        path.parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .file_name()
            .unwrap(),
        "humanize"
    );
}

#[test]
fn machine_input_ledger_runtime_default_path_sanitizes_run_id_segment() {
    let path = MachineInputLedger::runtime_default_path("../escape/run-a");
    let path_text = path.to_string_lossy();

    assert!(path_text.contains("/.cache/humanize/runs/"));
    assert!(!path_text.contains("/../"));
    assert_ne!(
        path.parent().unwrap().file_name().unwrap(),
        "../escape/run-a"
    );
}

#[test]
fn classifier_marks_matching_transaction_as_humanize_injected() {
    let records = vec![record("tx-a", "inspect the repo", 1_000)];
    let message = TranscriptMessage {
        role: TranscriptRole::User,
        text: "inspect the repo".to_string(),
        timestamp_ms: Some(1_000),
        transaction_id: Some("tx-a".to_string()),
    };

    let result =
        classify_transcript_message(&message, &records, &TranscriptClassifierConfig::default());

    assert_eq!(
        result,
        TranscriptClassification {
            classification: TranscriptInputClassification::HumanizeInjected,
            transaction_id: Some("tx-a".to_string()),
        }
    );
}

#[test]
fn classifier_marks_close_payload_match_without_transaction_as_likely_injected() {
    let records = vec![record("tx-a", "inspect the repo", 1_000)];
    let message = TranscriptMessage {
        role: TranscriptRole::User,
        text: "inspect the repo".to_string(),
        timestamp_ms: Some(1_200),
        transaction_id: None,
    };

    let result =
        classify_transcript_message(&message, &records, &TranscriptClassifierConfig::default());

    assert_eq!(
        result,
        TranscriptClassification {
            classification: TranscriptInputClassification::LikelyHumanizeInjected,
            transaction_id: Some("tx-a".to_string()),
        }
    );
}

#[test]
fn classifier_marks_unmatched_user_text_as_human_intervention() {
    let records = vec![record("tx-a", "inspect the repo", 1_000)];
    let message = TranscriptMessage {
        role: TranscriptRole::User,
        text: "manual follow up".to_string(),
        timestamp_ms: Some(1_200),
        transaction_id: None,
    };

    let result =
        classify_transcript_message(&message, &records, &TranscriptClassifierConfig::default());

    assert_eq!(
        result,
        TranscriptClassification {
            classification: TranscriptInputClassification::InferredHumanIntervention,
            transaction_id: None,
        }
    );
}

#[test]
fn classifier_consumes_matching_transactions_for_batch_diff() {
    let records = vec![record("tx-a", "inspect the repo", 1_000)];
    let messages = vec![
        TranscriptMessage {
            role: TranscriptRole::User,
            text: "inspect the repo".to_string(),
            timestamp_ms: Some(1_100),
            transaction_id: None,
        },
        TranscriptMessage {
            role: TranscriptRole::User,
            text: "inspect the repo".to_string(),
            timestamp_ms: Some(1_200),
            transaction_id: None,
        },
    ];

    let result =
        classify_transcript_messages(&messages, &records, &TranscriptClassifierConfig::default());

    assert_eq!(
        result,
        vec![
            TranscriptClassification {
                classification: TranscriptInputClassification::LikelyHumanizeInjected,
                transaction_id: Some("tx-a".to_string()),
            },
            TranscriptClassification {
                classification: TranscriptInputClassification::InferredHumanIntervention,
                transaction_id: None,
            },
        ]
    );
}

#[test]
fn classifier_prefers_submitted_record_for_duplicate_transaction_statuses() {
    let records = vec![
        record_with_status(
            "tx-a",
            "inspect the repo",
            1_000,
            MachineInputStatus::Started,
        ),
        record("tx-a", "inspect the repo", 1_010),
    ];
    let messages = vec![TranscriptMessage {
        role: TranscriptRole::User,
        text: "inspect the repo".to_string(),
        timestamp_ms: Some(1_011),
        transaction_id: None,
    }];

    let result =
        classify_transcript_messages(&messages, &records, &TranscriptClassifierConfig::default());

    assert_eq!(
        result,
        vec![TranscriptClassification {
            classification: TranscriptInputClassification::LikelyHumanizeInjected,
            transaction_id: Some("tx-a".to_string()),
        }]
    );
}

#[test]
fn classifier_does_not_treat_unsubmitted_records_as_machine_input() {
    let records = vec![
        record_with_status(
            "tx-started",
            "inspect the repo",
            1_000,
            MachineInputStatus::Started,
        ),
        record_with_status(
            "tx-failed",
            "retry the repo",
            1_200,
            MachineInputStatus::Failed,
        ),
    ];
    let messages = vec![
        TranscriptMessage {
            role: TranscriptRole::User,
            text: "inspect the repo".to_string(),
            timestamp_ms: Some(1_001),
            transaction_id: Some("tx-started".to_string()),
        },
        TranscriptMessage {
            role: TranscriptRole::User,
            text: "retry the repo".to_string(),
            timestamp_ms: Some(1_201),
            transaction_id: Some("tx-failed".to_string()),
        },
    ];

    let result =
        classify_transcript_messages(&messages, &records, &TranscriptClassifierConfig::default());

    assert_eq!(
        result,
        vec![
            TranscriptClassification {
                classification: TranscriptInputClassification::InferredHumanIntervention,
                transaction_id: None,
            },
            TranscriptClassification {
                classification: TranscriptInputClassification::InferredHumanIntervention,
                transaction_id: None,
            },
        ]
    );
}

#[test]
fn classifier_leaves_non_user_messages_unclassified() {
    let records = vec![record("tx-a", "inspect the repo", 1_000)];
    let message = TranscriptMessage {
        role: TranscriptRole::Assistant,
        text: "done".to_string(),
        timestamp_ms: Some(1_200),
        transaction_id: None,
    };

    let result =
        classify_transcript_message(&message, &records, &TranscriptClassifierConfig::default());

    assert_eq!(
        result,
        TranscriptClassification {
            classification: TranscriptInputClassification::Unclassified,
            transaction_id: None,
        }
    );
}
