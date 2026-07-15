use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::pipe_sink::PipeSinkIdentity;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{RunAssetError, read_regular_private};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct OwnedTmuxPane {
    pub run_id: String,
    pub activation_id: String,
    pub node_id: String,
    pub session_id: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
    pub allocation_generation: u64,
    pub tmux_target: String,
    pub session_ref: String,
    pub window_ref: String,
    pub pane_ref: String,
    pub tmux_target_ref: String,
    pub manifest_path: PathBuf,
}

impl OwnedTmuxPane {
    pub fn targets(&self) -> Vec<String> {
        let mut targets = vec![self.tmux_target.clone(), self.pane_id.clone()];
        let window_target = format!("{}:{}", self.session_id, self.window_id);
        if !self.session_id.is_empty() && !self.window_id.is_empty() {
            targets.push(window_target);
        }
        targets.extend([
            self.session_ref.clone(),
            self.window_ref.clone(),
            self.pane_ref.clone(),
            self.tmux_target_ref.clone(),
        ]);
        targets.retain(|target| !target.is_empty());
        targets.sort();
        targets.dedup();
        targets
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetTmuxTarget {
    pub session_id: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
    pub allocation_generation: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetActivationPaths {
    pub capture_root: PathBuf,
    pub metadata_path: PathBuf,
    pub pipe_path: PathBuf,
    pub pipe_relative_path: String,
    pub pipe_identity: PipeSinkIdentity,
    pub final_capture_path: PathBuf,
}

impl RunAssetTmuxTarget {
    pub fn target(&self) -> String {
        format!("{}:{}.{}", self.session_id, self.window_id, self.pane_id)
    }
}

pub fn discover_live_owned_tmux_panes_in_dir(
    root: &Path,
) -> Result<Vec<OwnedTmuxPane>, RunAssetError> {
    discover_owned_tmux_panes(root)
}

pub fn discover_private_owned_tmux_panes_in_dir(
    root: &Path,
) -> Result<Vec<OwnedTmuxPane>, RunAssetError> {
    discover_owned_tmux_panes(root)
}

fn discover_owned_tmux_panes(runs_root: &Path) -> Result<Vec<OwnedTmuxPane>, RunAssetError> {
    let mut owned = Vec::new();
    let runtime_root = crate::state_path::private_runtime_root()
        .map_err(|err| RunAssetError::new(err.to_string()))?;
    let identities =
        crate::private_state::discover_run_identities_for_runs_root(&runtime_root, runs_root)
            .map_err(|err| RunAssetError::new(err.to_string()))?;
    for identity in identities {
        let private_driver_dir =
            crate::state_path::private_run_root(&runtime_root, &identity.public_run_root)
                .join("driver");
        owned.extend(live_private_panes(
            &private_driver_dir,
            &identity.public_run_root,
            &identity.run_id,
        )?);
    }
    dedupe_owned_panes(&mut owned);
    owned.sort_by(|left, right| {
        left.run_id
            .cmp(&right.run_id)
            .then(left.activation_id.cmp(&right.activation_id))
            .then(left.pane_id.cmp(&right.pane_id))
    });
    Ok(owned)
}

fn dedupe_owned_panes(owned: &mut Vec<OwnedTmuxPane>) {
    let mut deduped = Vec::new();
    for pane in std::mem::take(owned) {
        if deduped.iter().any(|existing: &OwnedTmuxPane| {
            existing.run_id == pane.run_id
                && existing.activation_id == pane.activation_id
                && ((!existing.pane_id.is_empty() && existing.pane_id == pane.pane_id)
                    || (!existing.pane_ref.is_empty() && existing.pane_ref == pane.pane_ref))
        }) {
            continue;
        }
        deduped.push(pane);
    }
    *owned = deduped;
}

fn live_private_panes(
    private_driver_dir: &Path,
    public_run_root: &Path,
    run_id: &str,
) -> Result<Vec<OwnedTmuxPane>, RunAssetError> {
    let path = private_driver_dir.join("driver-events.jsonl");
    let manifest_path = public_run_root.join("manifest.json");
    let Some(bytes) = read_regular_private(&path)? else {
        return Ok(Vec::new());
    };
    let complete_tail = bytes.ends_with(b"\n");
    let lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    let mut driver_pane = None;
    let mut activation_panes = BTreeMap::<String, OwnedTmuxPane>::new();
    for (index, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let event = match serde_json::from_slice::<Value>(line) {
            Ok(event) => event,
            Err(_) if index + 1 == lines.len() && !complete_tail => break,
            Err(err) => {
                return Err(RunAssetError::new(format!(
                    "parse driver ownership event {} failed: {err}",
                    path.display()
                )));
            }
        };
        match event.get("kind").and_then(Value::as_str) {
            Some("driver_pane_owned") => {
                let stored = event.get("payload").and_then(|payload| payload.get("pane"));
                let Some(stored) = stored else {
                    continue;
                };
                driver_pane =
                    owned_pane_from_private(run_id, "driver", "driver", stored, &manifest_path);
            }
            Some("driver_pane_released") => driver_pane = None,
            Some("tmux_pane_allocated") => {
                let Some(payload) = event.get("payload") else {
                    continue;
                };
                let Some(activation_id) = payload.get("activation_id").and_then(Value::as_str)
                else {
                    continue;
                };
                let Some(stored) = payload.get("pane") else {
                    continue;
                };
                if let Some(pane) = owned_pane_from_private(
                    run_id,
                    activation_id,
                    activation_id,
                    stored,
                    &manifest_path,
                ) {
                    activation_panes.insert(activation_id.to_string(), pane);
                }
            }
            Some("tmux_pane_cleanup_receipt") => {
                if let Some(activation_id) = event
                    .get("payload")
                    .and_then(|payload| payload.get("activation_id"))
                    .and_then(Value::as_str)
                {
                    activation_panes.remove(activation_id);
                }
            }
            Some("tmux_panes_released") => {
                let Some(activation_ids) = event
                    .get("payload")
                    .and_then(|payload| payload.get("activation_ids"))
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                for activation_id in activation_ids.iter().filter_map(Value::as_str) {
                    activation_panes.remove(activation_id);
                }
            }
            _ => {}
        }
    }
    let mut panes = Vec::new();
    if let Some(pane) = driver_pane {
        panes.push(pane);
    }
    panes.extend(activation_panes.into_values());
    Ok(panes)
}

fn owned_pane_from_private(
    run_id: &str,
    activation_id: &str,
    node_id: &str,
    stored: &Value,
    manifest_path: &Path,
) -> Option<OwnedTmuxPane> {
    let pane_id = stored.get("pane_id").and_then(Value::as_str)?;
    let session_id = stored.get("session_id").and_then(Value::as_str)?;
    let window_id = stored.get("window_id").and_then(Value::as_str)?;
    let window_name = stored
        .get("window_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tmux_target = format!("{session_id}:{window_id}.{pane_id}");
    Some(OwnedTmuxPane {
        run_id: run_id.to_string(),
        activation_id: activation_id.to_string(),
        node_id: node_id.to_string(),
        session_id: session_id.to_string(),
        window_id: window_id.to_string(),
        window_name: window_name.to_string(),
        pane_id: pane_id.to_string(),
        allocation_generation: stored
            .get("allocation_generation")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        session_ref: public_hash_ref(session_id),
        window_ref: public_hash_ref(&format!("{session_id}:{window_id}")),
        pane_ref: public_hash_ref(pane_id),
        tmux_target_ref: public_hash_ref(&tmux_target),
        tmux_target,
        manifest_path: manifest_path.to_path_buf(),
    })
}

fn public_hash_ref(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}
