use std::collections::BTreeSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::adapters::tmux::{CommandRunner, TmuxPane};
use crate::driver::{private_driver_dir, runtime_root_for_run_root};
use crate::run_assets::append_private_line;

use super::McpServer;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct DriverCleanupOutcome {
    pub attempted: usize,
    pub failed: usize,
    pub diagnostics_persisted: bool,
}

impl DriverCleanupOutcome {
    pub(super) fn add_action(&mut self, failed: bool, diagnostics_persisted: bool) {
        self.attempted += 1;
        self.failed += usize::from(failed);
        self.diagnostics_persisted &= diagnostics_persisted;
    }
}

impl<R: CommandRunner> McpServer<R> {
    pub(super) fn cleanup_driver_response_panes(
        &mut self,
        run_root: &Path,
        response: &mut Value,
        additional_panes: &[TmuxPane],
        stage: &str,
    ) -> DriverCleanupOutcome {
        let mut panes = cleanup_panes_from_response(response);
        let physically_released = physically_released_panes_from_response(response);
        panes.retain(|pane| {
            !physically_released
                .iter()
                .any(|released| same_pane(released, pane))
        });
        for pane in additional_panes {
            if !physically_released
                .iter()
                .any(|released| same_pane(released, pane))
                && !panes.iter().any(|existing| same_pane(existing, pane))
            {
                panes.push(pane.clone());
            }
        }

        self.cleanup_driver_panes(run_root, response, &panes, stage)
    }

    pub(super) fn cleanup_driver_panes(
        &mut self,
        run_root: &Path,
        response: &mut Value,
        panes: &[TmuxPane],
        stage: &str,
    ) -> DriverCleanupOutcome {
        let mut unique_panes = Vec::new();
        for pane in panes {
            if !unique_panes
                .iter()
                .any(|existing| same_pane(existing, pane))
            {
                unique_panes.push(pane.clone());
            }
        }

        let mut details = Vec::new();
        let mut failed = 0;
        for pane in &unique_panes {
            let result = self.tmux_adapter.kill_pane(pane);
            if result.is_err() {
                failed += 1;
            }
            details.push(json!({
                "action": "kill_pane",
                "activation_id": pane.activation_id(),
                "session_id": pane.session_id(),
                "window_id": pane.window_id(),
                "pane_id": pane.id(),
                "result": if result.is_ok() { "complete" } else { "failed" },
                "error": result.err().map(|err| err.to_string())
            }));
        }

        let diagnostics_persisted = if details.is_empty() {
            true
        } else {
            persist_private_mcp_diagnostic(
                run_root,
                "tmux_cleanup",
                json!({
                    "stage": stage,
                    "actions": details
                }),
            )
        };
        sanitize_driver_error_response(response, &unique_panes);
        DriverCleanupOutcome {
            attempted: unique_panes.len(),
            failed,
            diagnostics_persisted,
        }
    }
}

pub(super) fn publish_cleanup_report(response: &mut Value, outcome: DriverCleanupOutcome) {
    if outcome.attempted == 0 {
        return;
    }
    let cleanup = json!({
        "attempted": outcome.attempted,
        "failed": outcome.failed,
        "status": if outcome.failed == 0 { "complete" } else { "incomplete" },
        "diagnostics_persisted": outcome.diagnostics_persisted
    });
    public_error_object(response).insert("cleanup".to_string(), cleanup);
}

pub(super) fn persist_private_mcp_diagnostic(run_root: &Path, kind: &str, details: Value) -> bool {
    let mut line = match serde_json::to_vec(&json!({
        "recorded_at_ms": now_ms(),
        "kind": kind,
        "details": details
    })) {
        Ok(line) => line,
        Err(_) => return false,
    };
    line.push(b'\n');
    let Ok(runtime_root) = runtime_root_for_run_root(run_root) else {
        return false;
    };
    let Ok(driver_dir) = private_driver_dir(&runtime_root, run_root) else {
        return false;
    };
    append_private_line(&driver_dir.join("mcp-diagnostics.jsonl"), &line).is_ok()
}

fn public_error_object(response: &mut Value) -> &mut Map<String, Value> {
    if response.get("error").is_some_and(Value::is_object) {
        return response
            .get_mut("error")
            .and_then(Value::as_object_mut)
            .expect("driver error must be an object");
    }
    response
        .as_object_mut()
        .expect("driver response must be an object")
}

fn sanitize_driver_error_response(response: &mut Value, panes: &[TmuxPane]) {
    let pane_tokens = panes
        .iter()
        .flat_map(|pane| {
            [
                pane.id().to_string(),
                format!("{}:{}.{}", pane.session_id(), pane.window_id(), pane.id()),
            ]
        })
        .collect::<Vec<_>>();
    sanitize_value(response, &pane_tokens);
}

pub(super) fn sanitize_public_driver_response(response: &mut Value) {
    let mut secrets = Vec::new();
    collect_private_response_values(response, None, &mut secrets);
    secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
    secrets.dedup();
    sanitize_public_response_value(response, &secrets);
}

fn collect_private_response_values(value: &Value, key: Option<&str>, secrets: &mut Vec<String>) {
    if key.is_some_and(is_private_response_field) {
        collect_string_values(value, secrets);
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                collect_private_response_values(child, Some(key), secrets);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_private_response_values(child, None, secrets);
            }
        }
        Value::String(text) if Path::new(text).is_absolute() => secrets.push(text.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn collect_string_values(value: &Value, secrets: &mut Vec<String>) {
    match value {
        Value::String(text) if !text.is_empty() => secrets.push(text.clone()),
        Value::Array(values) => {
            for child in values {
                collect_string_values(child, secrets);
            }
        }
        Value::Object(object) => {
            for child in object.values() {
                collect_string_values(child, secrets);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn sanitize_public_response_value(value: &mut Value, secrets: &[String]) {
    match value {
        Value::Object(object) => {
            object.retain(|key, _| !is_private_response_field(key));
            for child in object.values_mut() {
                sanitize_public_response_value(child, secrets);
            }
        }
        Value::Array(values) => {
            for child in values {
                sanitize_public_response_value(child, secrets);
            }
        }
        Value::String(text) => {
            for secret in secrets {
                if text.contains(secret) {
                    *text = text.replace(secret, "[redacted]");
                }
            }
            if Path::new(text.as_str()).is_absolute() {
                *text = "[redacted-path]".to_string();
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn is_private_response_field(key: &str) -> bool {
    let normalized = key
        .bytes()
        .filter(|byte| byte.is_ascii_alphanumeric())
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "agentcommand"
            | "argv"
            | "authtoken"
            | "authtokenpath"
            | "bindingfile"
            | "bindingpath"
            | "command"
            | "credential"
            | "executable"
            | "helperpid"
            | "manifestpath"
            | "metadatapath"
            | "nativesessionid"
            | "paneid"
            | "participantbindingpath"
            | "participantcredential"
            | "participanthandle"
            | "pipepath"
            | "readinessnonce"
            | "runroot"
            | "sessionid"
            | "socketpath"
            | "sourcenativeid"
            | "tmuxcleanup"
            | "tmuxtarget"
            | "token"
            | "transactionid"
            | "windowid"
            | "windowname"
    ) || normalized.ends_with("transactionid")
        || normalized.ends_with("credential")
        || normalized.ends_with("nonce")
}

fn sanitize_value(value: &mut Value, pane_tokens: &[String]) {
    match value {
        Value::Object(object) => {
            for key in [
                "argv",
                "cleanup_errors",
                "command",
                "pane",
                "pane_id",
                "panes",
                "run_root",
                "session_id",
                "tmux",
                "tmux_cleanup",
                "tmux_release_outcomes",
                "tmux_target",
                "window_id",
                "window_name",
            ] {
                object.remove(key);
            }
            for child in object.values_mut() {
                sanitize_value(child, pane_tokens);
            }
        }
        Value::Array(values) => {
            for child in values {
                sanitize_value(child, pane_tokens);
            }
        }
        Value::String(text) => {
            for token in pane_tokens {
                if !token.is_empty() && text.contains(token) {
                    *text = text.replace(token, "[redacted-pane]");
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn same_pane(left: &TmuxPane, right: &TmuxPane) -> bool {
    left.session_id() == right.session_id()
        && left.window_id() == right.window_id()
        && left.id() == right.id()
}

pub(super) fn cleanup_panes_from_response(response: &Value) -> Vec<TmuxPane> {
    let mut panes = Vec::new();
    let mut seen = BTreeSet::new();
    let mut containers = vec![response];
    if let Some(error) = response.get("error") {
        containers.push(error);
    }
    for container in containers {
        for key in ["tmux_cleanup", "tmux"] {
            let Some(values) = container
                .get(key)
                .and_then(|value| value.get("panes"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for value in values {
                if let Some(pane) = pane_from_value(value)
                    && seen.insert((
                        pane.session_id().to_string(),
                        pane.window_id().to_string(),
                        pane.id().to_string(),
                    ))
                {
                    panes.push(pane);
                }
            }
        }
    }
    panes
}

pub(super) fn physically_released_panes_from_response(response: &Value) -> Vec<TmuxPane> {
    let mut panes = Vec::new();
    let mut containers = vec![response];
    if let Some(error) = response.get("error") {
        containers.push(error);
    }
    for container in containers {
        let Some(outcomes) = container
            .get("tmux_release_outcomes")
            .and_then(Value::as_array)
        else {
            continue;
        };
        for outcome in outcomes {
            if outcome.get("physical_release").and_then(Value::as_str) != Some("complete") {
                continue;
            }
            let Some(pane) = outcome.get("pane").and_then(pane_from_value) else {
                continue;
            };
            if !panes.iter().any(|existing| same_pane(existing, &pane)) {
                panes.push(pane);
            }
        }
    }
    panes
}

fn pane_from_value(value: &Value) -> Option<TmuxPane> {
    let session_id = value
        .get("session_id")
        .or_else(|| value.get("session"))
        .and_then(Value::as_str)?;
    let window_id = value
        .get("window_id")
        .or_else(|| value.get("window"))
        .and_then(Value::as_str)?;
    let pane_id = value.get("pane_id").and_then(Value::as_str)?;
    let activation_id = value
        .get("activation_id")
        .and_then(Value::as_str)
        .unwrap_or("driver-cleanup");
    Some(TmuxPane::new_in_session(
        session_id,
        window_id,
        activation_id,
        pane_id,
    ))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
