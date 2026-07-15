use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use serde_json::{Value, json};

use crate::adapters::tmux::TmuxPipeCaptureDescriptor;
use crate::run_assets::read_regular_private;
use crate::runtime;

use super::delivery::{
    AmbiguousDelivery, DELIVERY_ROLE_AGENT_LAUNCH, DELIVERY_ROLE_NODE_PROMPT,
    DELIVERY_ROLE_PARTICIPANT_MESSAGE, SubmittedDelivery, delivery_key,
};
use super::flow_lock::StoredFlowRevision;
use super::participant::ParticipantBinding;
use super::run_lifecycle::{TmuxPaneCleanupIntent, TmuxPipeCaptureIntent};
use super::storage::{
    atomic_write_private_json, contained_relative_path, read_jsonl_recover_torn_tail,
    safe_file_segment,
};
use super::{
    DRIVER_EVENTS_FILE, DriverDurableEvent, DriverFailure, DriverTmuxState,
    RUNTIME_EVENT_BATCH_PROTOCOL, RuntimeDriverService, SNAPSHOT_FILE, StoredPane,
    TmuxPaneAllocationIntent,
};

pub(super) fn read_runtime_events(run_root: &Path) -> io::Result<Vec<runtime::Event>> {
    let path = run_root.join("driver").join(super::EVENTS_FILE);
    let records = read_jsonl_recover_torn_tail::<Value>(&path)?;
    let mut events = Vec::new();
    for record in records {
        if record.get("protocol").and_then(Value::as_str) == Some(RUNTIME_EVENT_BATCH_PROTOCOL) {
            let base_event_count = record
                .get("base_event_count")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "runtime event batch base_event_count is required",
                    )
                })?;
            if base_event_count != events.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "runtime event batch base_event_count mismatch",
                ));
            }
            let batch_events = record.get("events").cloned().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "runtime event batch events are required",
                )
            })?;
            let batch_events = serde_json::from_value::<Vec<runtime::Event>>(batch_events)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            events.extend(batch_events);
        } else {
            events.push(
                serde_json::from_value::<runtime::Event>(record)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
            );
        }
    }
    Ok(events)
}

pub(super) fn read_driver_events(run_root: &Path) -> io::Result<Vec<DriverDurableEvent>> {
    let path = run_root.join("driver").join(DRIVER_EVENTS_FILE);
    read_jsonl_recover_torn_tail(&path)
}

pub(super) fn read_runtime_referenced_locks(
    run_root: &Path,
    events: &[runtime::Event],
) -> io::Result<BTreeMap<String, StoredFlowRevision>> {
    let mut references = BTreeMap::<String, String>::new();
    for event in events {
        match &event.payload {
            runtime::EventPayload::FlowApplied {
                lock_id,
                content_hash,
                ..
            } => {
                references.insert(lock_id.clone(), content_hash.clone());
            }
            runtime::EventPayload::FlowUpdate {
                lock_id,
                contract_hash,
                ..
            } => {
                references.insert(lock_id.clone(), contract_hash.clone());
            }
            _ => {}
        }
    }

    let mut locks = BTreeMap::new();
    for (lock_id, content_hash) in references {
        let path = run_root
            .join("driver")
            .join(super::REVISIONS_DIR)
            .join(format!("{}.json", safe_file_segment(&content_hash)));
        let bytes = read_regular_private(&path)
            .map_err(|err| io::Error::other(err.to_string()))?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("flow revision file {} is missing", path.display()),
                )
            })?;
        let package = serde_json::from_slice::<StoredFlowRevision>(&bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        if package.lock_id() != lock_id || package.content_hash() != content_hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "runtime flow revision reference does not match immutable package",
            ));
        }
        locks.insert(lock_id, package);
    }
    Ok(locks)
}

pub(super) struct ReplayedDriverEvents {
    pub(super) locks: BTreeMap<String, StoredFlowRevision>,
    pub(super) tmux: Option<DriverTmuxState>,
    pub(super) operator_pane: Option<StoredPane>,
    pub(super) pipe_captures: BTreeMap<String, (u64, TmuxPipeCaptureDescriptor)>,
    pub(super) agent_launch_submitted_activations: BTreeSet<(String, u64)>,
    pub(super) settled_actuation_activations: BTreeSet<(String, u64)>,
    pub(super) ambiguous_deliveries: BTreeMap<(String, String), AmbiguousDelivery>,
    pub(super) submitted_deliveries: BTreeMap<(String, String), SubmittedDelivery>,
    pub(super) allocation_generations: BTreeMap<String, u64>,
    pub(super) pending_tmux_allocations: BTreeMap<String, TmuxPaneAllocationIntent>,
    pub(super) pending_pipe_captures: BTreeMap<String, TmuxPipeCaptureIntent>,
    pub(super) pending_tmux_cleanups: BTreeMap<String, TmuxPaneCleanupIntent>,
    pub(super) participant_bindings: BTreeMap<(String, u64), ParticipantBinding>,
}

pub(super) fn replay_driver_events(
    run_root: &Path,
    events: &[DriverDurableEvent],
) -> io::Result<ReplayedDriverEvents> {
    let mut locks = BTreeMap::new();
    let mut tmux = None::<DriverTmuxState>;
    let mut operator_pane = None::<StoredPane>;
    let mut pipe_captures = BTreeMap::new();
    let mut agent_launch_submitted_activations = BTreeSet::new();
    let mut settled_actuation_activations = BTreeSet::new();
    let mut ambiguous_deliveries = BTreeMap::new();
    let mut submitted_deliveries = BTreeMap::new();
    let mut allocation_generations = BTreeMap::<String, u64>::new();
    let mut pending_tmux_allocations = BTreeMap::<String, TmuxPaneAllocationIntent>::new();
    let mut pending_pipe_captures = BTreeMap::<String, TmuxPipeCaptureIntent>::new();
    let mut pending_tmux_cleanups = BTreeMap::<String, TmuxPaneCleanupIntent>::new();
    let mut participant_bindings = BTreeMap::<(String, u64), ParticipantBinding>::new();
    for event in events {
        match event.kind.as_str() {
            "driver_pane_owned" => {
                if let Some(pane) = event.payload.get("pane") {
                    operator_pane = Some(
                        serde_json::from_value::<StoredPane>(pane.clone())
                            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
                    );
                }
            }
            "driver_pane_released" => operator_pane = None,
            "pipe_capture_intent" => {
                let intent = serde_json::from_value::<TmuxPipeCaptureIntent>(event.payload.clone())
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                pending_pipe_captures.insert(intent.activation_id.clone(), intent);
            }
            "pipe_capture_started" => {
                if let (Some(activation_id), Some(descriptor)) = (
                    event.payload.get("activation_id").and_then(Value::as_str),
                    event.payload.get("descriptor"),
                ) {
                    let descriptor =
                        serde_json::from_value::<TmuxPipeCaptureDescriptor>(descriptor.clone())
                            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                    let allocation_generation = event
                        .payload
                        .get("allocation_generation")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    pipe_captures.insert(
                        activation_id.to_string(),
                        (allocation_generation, descriptor),
                    );
                    pending_pipe_captures.remove(activation_id);
                }
            }
            "flow_revision_available" => {
                let Some(lock_id) = event.payload.get("lock_id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(revision_file) =
                    event.payload.get("revision_file").and_then(Value::as_str)
                else {
                    continue;
                };
                let path = contained_relative_path(run_root, revision_file)?;
                let bytes = read_regular_private(&path)
                    .map_err(|err| io::Error::other(err.to_string()))?
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            format!("flow revision file {} is missing", path.display()),
                        )
                    })?;
                let package = serde_json::from_slice::<StoredFlowRevision>(&bytes)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                locks.insert(lock_id.to_string(), package);
            }
            "tmux_bound" => {
                tmux = Some(DriverTmuxState {
                    session_id: event
                        .payload
                        .get("session_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    window_name: event
                        .payload
                        .get("window_name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    window_id: event
                        .payload
                        .get("window_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    agent_command: event
                        .payload
                        .get("agent_command")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    actuation: event
                        .payload
                        .get("actuation")
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?
                        .unwrap_or_default(),
                    panes: BTreeMap::new(),
                });
            }
            "tmux_pane_allocation_intent" => {
                let intent =
                    serde_json::from_value::<TmuxPaneAllocationIntent>(event.payload.clone())
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                pending_tmux_allocations.insert(intent.activation_id.clone(), intent);
            }
            "tmux_pane_allocated" => {
                let Some(tmux) = tmux.as_mut() else {
                    continue;
                };
                let Some(activation_id) =
                    event.payload.get("activation_id").and_then(Value::as_str)
                else {
                    continue;
                };
                let Some(pane) = event.payload.get("pane") else {
                    continue;
                };
                let mut pane = serde_json::from_value::<StoredPane>(pane.clone())
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                let generation = event
                    .payload
                    .get("pane")
                    .and_then(|pane| pane.get("allocation_generation"))
                    .and_then(Value::as_u64)
                    .unwrap_or_else(|| {
                        allocation_generations
                            .get(activation_id)
                            .map_or(0, |generation| generation.saturating_add(1))
                    });
                pane.allocation_generation = generation;
                allocation_generations.insert(activation_id.to_string(), generation);
                tmux.panes.insert(activation_id.to_string(), pane);
                pending_tmux_allocations.remove(activation_id);
            }
            "tmux_pane_cleanup_intent" => {
                let intent = serde_json::from_value::<TmuxPaneCleanupIntent>(event.payload.clone())
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                pending_tmux_cleanups.insert(intent.activation_id.clone(), intent);
            }
            "tmux_pane_cleanup_receipt" => {
                let receipt =
                    serde_json::from_value::<TmuxPaneCleanupIntent>(event.payload.clone())
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                if pending_tmux_cleanups
                    .get(&receipt.activation_id)
                    .is_some_and(|intent| intent == &receipt)
                {
                    pending_tmux_cleanups.remove(&receipt.activation_id);
                }
            }
            "tmux_panes_released" => {
                let Some(tmux) = tmux.as_mut() else {
                    continue;
                };
                let Some(activation_ids) = event
                    .payload
                    .get("activation_ids")
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                for activation_id in activation_ids.iter().filter_map(Value::as_str) {
                    tmux.panes.remove(activation_id);
                    pending_tmux_allocations.remove(activation_id);
                    pending_pipe_captures.remove(activation_id);
                    pipe_captures.remove(activation_id);
                    agent_launch_submitted_activations.retain(|(submitted_activation_id, _)| {
                        submitted_activation_id != activation_id
                    });
                    settled_actuation_activations.retain(|(settled_activation_id, _)| {
                        settled_activation_id != activation_id
                    });
                }
                if let Some(barriers) = event
                    .payload
                    .get("delivery_barriers")
                    .and_then(Value::as_array)
                {
                    for barrier in barriers {
                        let Some(delivery) = AmbiguousDelivery::from_payload(barrier, event.seq)
                        else {
                            continue;
                        };
                        let key = delivery.key();
                        submitted_deliveries.remove(&key);
                        ambiguous_deliveries.insert(key, delivery);
                    }
                }
            }
            "input_delivery_started" => {
                let Some(delivery) = AmbiguousDelivery::from_payload(&event.payload, event.seq)
                else {
                    continue;
                };
                let key = delivery.key();
                submitted_deliveries.remove(&key);
                ambiguous_deliveries.insert(key, delivery);
            }
            "input_delivery_not_submitted" => {
                if let (Some(activation_id), Some(role)) = (
                    event.payload.get("activation_id").and_then(Value::as_str),
                    event.payload.get("role").and_then(Value::as_str),
                ) {
                    let key = delivery_key(
                        activation_id,
                        role,
                        event.payload.get("message_id").and_then(Value::as_str),
                        event
                            .payload
                            .get("allocation_generation")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    );
                    if delivery_event_matches(&ambiguous_deliveries, &key, &event.payload, true) {
                        ambiguous_deliveries.remove(&key);
                        submitted_deliveries.remove(&key);
                    }
                }
            }
            "agent_launch_submitted" | "agent_launched" => {
                if let Some(activation_id) =
                    event.payload.get("activation_id").and_then(Value::as_str)
                {
                    let key = delivery_key(
                        activation_id,
                        DELIVERY_ROLE_AGENT_LAUNCH,
                        None,
                        event
                            .payload
                            .get("allocation_generation")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    );
                    let legacy = event.kind == "agent_launched";
                    if delivery_event_matches(&ambiguous_deliveries, &key, &event.payload, legacy) {
                        let submission =
                            ambiguous_deliveries.get(&key).map(SubmittedDelivery::from);
                        ambiguous_deliveries.remove(&key);
                        if let Some(submission) = submission {
                            submitted_deliveries.insert(key, submission);
                        }
                        let allocation_generation = event
                            .payload
                            .get("allocation_generation")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        agent_launch_submitted_activations
                            .insert((activation_id.to_string(), allocation_generation));
                    }
                }
            }
            "prompt_submitted" | "node_prompt_sent" => {
                if let Some(activation_id) =
                    event.payload.get("activation_id").and_then(Value::as_str)
                {
                    let key = delivery_key(
                        activation_id,
                        DELIVERY_ROLE_NODE_PROMPT,
                        None,
                        event
                            .payload
                            .get("allocation_generation")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    );
                    let legacy = event.kind == "node_prompt_sent";
                    if delivery_event_matches(&ambiguous_deliveries, &key, &event.payload, legacy) {
                        let submission =
                            ambiguous_deliveries.get(&key).map(SubmittedDelivery::from);
                        ambiguous_deliveries.remove(&key);
                        if let Some(submission) = submission {
                            submitted_deliveries.insert(key, submission);
                        }
                        let allocation_generation = event
                            .payload
                            .get("allocation_generation")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        agent_launch_submitted_activations
                            .insert((activation_id.to_string(), allocation_generation));
                        settled_actuation_activations
                            .insert((activation_id.to_string(), allocation_generation));
                    }
                }
            }
            "participant_message_submitted" => {
                if let (Some(activation_id), Some(message_id)) = (
                    event.payload.get("activation_id").and_then(Value::as_str),
                    event.payload.get("message_id").and_then(Value::as_str),
                ) {
                    let key = delivery_key(
                        activation_id,
                        DELIVERY_ROLE_PARTICIPANT_MESSAGE,
                        Some(message_id),
                        event
                            .payload
                            .get("allocation_generation")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    );
                    if delivery_event_matches(&ambiguous_deliveries, &key, &event.payload, false) {
                        let submission =
                            ambiguous_deliveries.get(&key).map(SubmittedDelivery::from);
                        ambiguous_deliveries.remove(&key);
                        if let Some(submission) = submission {
                            submitted_deliveries.insert(key, submission);
                        }
                    }
                }
            }
            "participant_started" => {
                let Some(binding) = event.payload.get("binding") else {
                    continue;
                };
                let binding = serde_json::from_value::<ParticipantBinding>(binding.clone())
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                participant_bindings.insert(binding.key(), binding);
            }
            "participant_bound" => {
                let Some(activation_id) =
                    event.payload.get("activation_id").and_then(Value::as_str)
                else {
                    continue;
                };
                let allocation_generation = event
                    .payload
                    .get("allocation_generation")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let Some(binding) = participant_bindings
                    .get_mut(&(activation_id.to_string(), allocation_generation))
                else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "participant bound event has no started binding",
                    ));
                };
                binding.native_session_id = event
                    .payload
                    .get("native_session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                binding.platform = event
                    .payload
                    .get("platform")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                binding.source = event
                    .payload
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            _ => {}
        }
    }
    Ok(ReplayedDriverEvents {
        locks,
        tmux,
        operator_pane,
        pipe_captures,
        agent_launch_submitted_activations,
        settled_actuation_activations,
        ambiguous_deliveries,
        submitted_deliveries,
        allocation_generations,
        pending_tmux_allocations,
        pending_pipe_captures,
        pending_tmux_cleanups,
        participant_bindings,
    })
}

fn delivery_event_matches(
    deliveries: &BTreeMap<(String, String), AmbiguousDelivery>,
    key: &(String, String),
    payload: &Value,
    allow_missing_sequence: bool,
) -> bool {
    match payload
        .get("started_event_sequence")
        .and_then(Value::as_u64)
    {
        Some(sequence) => deliveries
            .get(key)
            .is_some_and(|delivery| delivery.started_event_sequence == sequence),
        None => allow_missing_sequence,
    }
}

impl RuntimeDriverService {
    pub(super) fn commit_runtime(
        &mut self,
        next_driver: runtime::DriverState,
    ) -> Result<(), DriverFailure> {
        self.commit_runtime_with_publication(next_driver, &[])
    }

    pub(super) fn write_snapshot(&self) -> Result<(), DriverFailure> {
        if let Some(path) = std::env::var_os("HUMANIZE_DRIVER_FAIL_SNAPSHOT_IF_EXISTS")
            && std::path::PathBuf::from(path).exists()
        {
            return Err(DriverFailure::new(
                "persistence_failed",
                "injected snapshot persistence failure",
            ));
        }
        let context = self.authoritative_context_for_cache()?;
        atomic_write_private_json(&self.driver_dir().join(SNAPSHOT_FILE), &context)
    }

    pub(super) fn fail_runtime_append_if_requested(
        &self,
        events: &[runtime::Event],
    ) -> Result<(), DriverFailure> {
        let Some(fault_at) = std::env::var("HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_AT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
        else {
            return Ok(());
        };
        let Some(marker) = std::env::var_os("HUMANIZE_DRIVER_FAIL_RUNTIME_APPEND_IF_EXISTS") else {
            return Ok(());
        };
        if std::path::PathBuf::from(marker).exists() && events.len() >= fault_at {
            return Err(DriverFailure::new(
                "persistence_failed",
                format!("injected runtime event append failure at event {fault_at}"),
            ));
        }
        Ok(())
    }

    pub(super) fn write_lock_revision(
        &self,
        lock_id: &str,
        content_hash: &str,
        package: &StoredFlowRevision,
    ) -> Result<(), DriverFailure> {
        if let Some(existing) = self.locks.get(lock_id) {
            let existing_value = serde_json::to_value(existing)
                .map_err(|err| DriverFailure::new("persistence_failed", err.to_string()))?;
            let package_value = serde_json::to_value(package)
                .map_err(|err| DriverFailure::new("persistence_failed", err.to_string()))?;
            if existing_value != package_value {
                return Err(DriverFailure::new(
                    "revision_hash_conflict",
                    "immutable flow revision package content mismatch",
                ));
            }
            return Ok(());
        }
        let revision_path = self
            .revisions_dir()
            .join(format!("{}.json", safe_file_segment(content_hash)));
        let existing = read_regular_private(&revision_path)
            .map_err(|err| DriverFailure::new("persistence_failed", err.to_string()))?;
        if let Some(existing) = existing {
            let existing = String::from_utf8(existing)
                .map_err(|err| DriverFailure::new("persistence_failed", err.to_string()))?;
            let expected = serde_json::to_string_pretty(package)
                .map_err(|err| DriverFailure::new("persistence_failed", err.to_string()))?;
            if existing.trim() != expected.trim() {
                return Err(DriverFailure::new(
                    "revision_hash_conflict",
                    "immutable flow revision file content mismatch",
                ));
            }
        } else {
            atomic_write_private_json(&revision_path, package)?;
        }
        Ok(())
    }

    pub(super) fn publish_applied_lock_revision(
        &mut self,
        lock_id: &str,
        content_hash: &str,
        package: &StoredFlowRevision,
    ) -> Result<(), DriverFailure> {
        if self.locks.contains_key(lock_id) {
            let _ = self.write_snapshot();
            return Ok(());
        }
        let relative = self.revision_relative_path(content_hash);
        self.append_driver_event(
            "flow_revision_available",
            json!({
                "lock_id": lock_id,
                "content_hash": content_hash,
                "revision_file": relative,
                "review_id": package.review_id()
            }),
        )?;
        self.locks.insert(lock_id.to_string(), package.clone());
        let _ = self.write_snapshot();
        Ok(())
    }

    fn revision_relative_path(&self, content_hash: &str) -> String {
        let revision_path = self
            .revisions_dir()
            .join(format!("{}.json", safe_file_segment(content_hash)));
        revision_path
            .strip_prefix(&self.private_run_root)
            .unwrap_or(&revision_path)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use crate::flow::{self, FlowCheckMode, FlowDraft, FlowNode, FlowResource, ResourceKind};
    use crate::run_assets::{RunAssetSink, RunAssetStore};
    use crate::runtime::{self, EventKind, EventPayload, EventSource, EventStrength, FlowLockMode};

    use super::super::flow_lock::StoredFlowRevision;
    use super::super::storage::safe_file_segment;
    use super::super::{DriverConfig, DriverDurableEvent, RuntimeDriverService};
    use super::{read_runtime_referenced_locks, replay_driver_events};

    #[derive(Debug, Clone, Copy)]
    enum RevisionAttack {
        Symlink,
        HardLink,
        PublicMode,
        Fifo,
    }

    #[test]
    fn every_revision_reader_rejects_symlink_hardlink_and_public_mode() {
        for operation in ["runtime", "replay", "existing_write"] {
            for attack in [
                RevisionAttack::Symlink,
                RevisionAttack::HardLink,
                RevisionAttack::PublicMode,
            ] {
                let root = test_root(&format!("revision-{operation}-{attack:?}"));
                assert!(
                    revision_operation_rejected(&root, operation, attack),
                    "{operation} accepted {attack:?} revision substitution"
                );
                fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn every_revision_reader_rejects_fifo_without_blocking() {
        for operation in ["runtime", "replay", "existing_write"] {
            let root = test_root(&format!("revision-fifo-{operation}"));
            let mut child = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("driver::persistence::tests::revision_fifo_child")
                .env("HUMANIZE_TEST_REVISION_ROOT", &root)
                .env("HUMANIZE_TEST_REVISION_OPERATION", operation)
                .spawn()
                .unwrap();
            let started = Instant::now();
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if started.elapsed() >= Duration::from_secs(1) {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    fs::remove_dir_all(&root).unwrap();
                    panic!("{operation} blocked on a FIFO revision");
                }
                thread::sleep(Duration::from_millis(10));
            };
            assert!(status.success(), "{operation} accepted a FIFO revision");
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn revision_fifo_child() {
        let Ok(root) = std::env::var("HUMANIZE_TEST_REVISION_ROOT") else {
            return;
        };
        let operation = std::env::var("HUMANIZE_TEST_REVISION_OPERATION").unwrap();
        assert!(revision_operation_rejected(
            Path::new(&root),
            &operation,
            RevisionAttack::Fifo,
        ));
    }

    fn revision_operation_rejected(root: &Path, operation: &str, attack: RevisionAttack) -> bool {
        let run_id = format!("run-{operation}");
        let runs_root = root.join("runs");
        let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(runs_root.clone()));
        let manifest = store.start_run_manifest(&run_id).unwrap();
        let run_root = manifest.root;
        let runtime_root = root.join("runtime");
        fs::create_dir_all(&runtime_root).unwrap();
        set_private_dir(&runtime_root);
        let private_run_root = private_run_root_for_run_root(&runtime_root, &run_root);
        let driver_dir = private_run_root.join("driver");
        let revisions_dir = driver_dir.join("revisions");
        fs::create_dir_all(&revisions_dir).unwrap();
        set_private_dir(&private_run_root);
        set_private_dir(&driver_dir);
        set_private_dir(&revisions_dir);

        let package = lock_package();
        let lock_id = package.lock_id().to_string();
        let content_hash = package.content_hash().to_string();
        let revision_file = format!("{}.json", safe_file_segment(&content_hash));
        let revision_path = revisions_dir.join(&revision_file);
        install_revision_attack(&revision_path, &package, attack);

        match operation {
            "runtime" => read_runtime_referenced_locks(
                &private_run_root,
                &[runtime_flow_event(&run_id, &lock_id, &content_hash)],
            )
            .is_err(),
            "replay" => replay_driver_events(
                &private_run_root,
                &[DriverDurableEvent {
                    seq: 1,
                    at_ms: 1,
                    kind: "flow_revision_available".to_string(),
                    payload: json!({
                        "lock_id": lock_id,
                        "content_hash": content_hash,
                        "revision_file": format!("driver/revisions/{revision_file}")
                    }),
                }],
            )
            .is_err(),
            "existing_write" => {
                let service = RuntimeDriverService::load(DriverConfig {
                    run_id,
                    runs_root,
                    runtime_root,
                    auth_token: "test-token".to_string(),
                    auth_token_path: None,
                    review_root: root.join("reviews"),
                    operator_pane: None,
                })
                .unwrap();
                service
                    .write_lock_revision(&lock_id, &content_hash, &package)
                    .is_err()
            }
            _ => panic!("unknown revision operation"),
        }
    }

    fn private_run_root_for_run_root(runtime_root: &Path, run_root: &Path) -> PathBuf {
        let identity = std::path::absolute(run_root)
            .unwrap_or_else(|_| run_root.to_path_buf())
            .to_string_lossy()
            .into_owned();
        runtime_root.join(format!("r{:016x}", stable_hash(&identity)))
    }

    fn stable_hash(input: &str) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in input.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn lock_package() -> StoredFlowRevision {
        let draft = FlowDraft {
            nodes: vec![FlowNode {
                id: "root".to_string(),
                ..FlowNode::default()
            }],
            resources: vec![FlowResource {
                id: "README.md".to_string(),
                kind: ResourceKind::Readme,
                source: "Revision persistence fixture.".to_string(),
            }],
            ..FlowDraft::default()
        };
        let lock = flow::flow_lock(&draft, FlowCheckMode::Core).unwrap();
        StoredFlowRevision::for_test(lock, "review-revision")
    }

    fn runtime_flow_event(run_id: &str, lock_id: &str, content_hash: &str) -> runtime::Event {
        runtime::Event {
            sequence: 1,
            source: EventSource {
                run_id: Some(run_id.to_string()),
                activation_id: None,
                source_id: None,
            },
            kind: EventKind::FlowApplied,
            strength: EventStrength::Applied,
            actor: None,
            correlation: None,
            payload: EventPayload::FlowApplied {
                run_id: run_id.to_string(),
                mode: FlowLockMode::FutureActivations,
                lock_id: lock_id.to_string(),
                content_hash: content_hash.to_string(),
            },
        }
    }

    fn install_revision_attack(
        revision_path: &Path,
        package: &StoredFlowRevision,
        attack: RevisionAttack,
    ) {
        let mut bytes = serde_json::to_vec_pretty(package).unwrap();
        bytes.push(b'\n');
        match attack {
            RevisionAttack::Symlink => {
                let target = revision_path.with_extension("target");
                fs::write(&target, bytes).unwrap();
                fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
                symlink(&target, revision_path).unwrap();
            }
            RevisionAttack::HardLink => {
                let target = revision_path.with_extension("target");
                fs::write(&target, bytes).unwrap();
                fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
                fs::hard_link(target, revision_path).unwrap();
            }
            RevisionAttack::PublicMode => {
                fs::write(revision_path, bytes).unwrap();
                fs::set_permissions(revision_path, fs::Permissions::from_mode(0o644)).unwrap();
            }
            RevisionAttack::Fifo => {
                let fifo = CString::new(revision_path.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            }
        }
    }

    fn set_private_dir(path: &Path) {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn test_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "humanize-driver-persistence-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        set_private_dir(&root);
        root
    }
}
