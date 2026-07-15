use std::fs::File;
use std::io::{self, Read};

use serde_json::{Value, json};

use crate::driver::DriverClient;
use crate::participant_binding::ParticipantBindingFile;
use crate::run_assets::RunAssetStore;

pub fn run_session_start_hook(source: &str, reader: &mut impl Read) -> io::Result<()> {
    let Some((_, binding)) = ParticipantBindingFile::from_environment()? else {
        return Ok(());
    };
    validate_current_pane(&binding)?;
    let mut input = String::new();
    reader.read_to_string(&mut input)?;
    let event = serde_json::from_str::<Value>(&input)
        .map_err(|err| invalid_input(format!("parse SessionStart hook JSON failed: {err}")))?;
    if event.get("hook_event_name").and_then(Value::as_str) != Some("SessionStart") {
        return Err(invalid_input("SessionStart hook event is required"));
    }
    let native_session_id = native_session_id(&event)?;
    let platform = platform_for_source(source)?;
    let store = RunAssetStore::new(crate::run_assets::RunAssetSink::HumanizeRunsDir(
        binding.runs_root.clone(),
    ));
    let run_root = store
        .run_root(&binding.run_id)
        .map_err(|err| invalid_input(err.to_string()))?;
    let client = DriverClient::from_run_root_for_run(&run_root, &binding.run_id)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "runtime driver unavailable"))?;
    let response = client.request(
        "participant_bind",
        &binding.run_id,
        &json!({
            "activation_id": binding.activation_id,
            "allocation_generation": binding.allocation_generation,
            "pane_id": binding.pane_id,
            "readiness_nonce": binding.readiness_nonce,
            "participant_handle": binding.handle,
            "participant_credential": binding.credential,
            "native_session_id": native_session_id,
            "platform": platform,
            "source": source
        }),
    )?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = response
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("participant binding failed");
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, message));
    }
    Ok(())
}

pub fn run_stop_hook(platform: &str, reader: &mut impl Read) -> io::Result<Value> {
    let (_, binding) = ParticipantBindingFile::from_environment()?.ok_or_else(|| {
        invalid_input("participant Stop hook requires a complete binding environment")
    })?;
    validate_current_pane(&binding)?;
    let mut input = String::new();
    reader.read_to_string(&mut input)?;
    let event = serde_json::from_str::<Value>(&input)
        .map_err(|err| invalid_input(format!("parse Stop hook JSON failed: {err}")))?;
    if event.get("hook_event_name").and_then(Value::as_str) != Some("Stop") {
        return Err(invalid_input("Stop hook event is required"));
    }
    let native_session_id = native_session_id(&event)?;
    let invocation_id = new_stop_invocation_id()?;
    let store = RunAssetStore::new(crate::run_assets::RunAssetSink::HumanizeRunsDir(
        binding.runs_root.clone(),
    ));
    let run_root = store
        .run_root(&binding.run_id)
        .map_err(|err| invalid_input(err.to_string()))?;
    let client = DriverClient::from_run_root_for_run(&run_root, &binding.run_id)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "runtime driver unavailable"))?;
    let response = client.request_participant_stop_with_one_ambiguous_retry(
        &binding.run_id,
        &json!({
            "activation_id": binding.activation_id,
            "participant_handle": binding.handle,
            "participant_credential": binding.credential,
            "native_session_id": native_session_id,
            "invocation_id": invocation_id,
            "reason": "participant requested stop"
        }),
    )?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = response
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("participant stop validation failed");
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, message));
    }
    if !matches!(platform, "codex" | "claude") {
        return Err(invalid_input("unsupported Stop hook platform"));
    }
    if response.get("hook_action").and_then(Value::as_str) == Some("allow") {
        Ok(json!({}))
    } else {
        Ok(json!({
            "decision": "block",
            "reason": missing_stop_feedback(&response)
        }))
    }
}

pub fn run_participant_exited_hook(exit_status: i32) -> io::Result<()> {
    let (_, binding) = ParticipantBindingFile::from_environment()?.ok_or_else(|| {
        invalid_input("participant exit hook requires a complete binding environment")
    })?;
    let store = RunAssetStore::new(crate::run_assets::RunAssetSink::HumanizeRunsDir(
        binding.runs_root.clone(),
    ));
    let run_root = store
        .run_root(&binding.run_id)
        .map_err(|err| invalid_input(err.to_string()))?;
    let client = DriverClient::from_run_root_for_run(&run_root, &binding.run_id)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "runtime driver unavailable"))?;
    let response = client.request(
        "participant_exited",
        &binding.run_id,
        &json!({
            "activation_id": binding.activation_id,
            "participant_handle": binding.handle,
            "participant_credential": binding.credential,
            "exit_status": exit_status
        }),
    )?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = response
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("participant exit observation failed");
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, message));
    }
    Ok(())
}

fn native_session_id(event: &Value) -> io::Result<&str> {
    ["session_id", "sessionId", "thread_id", "conversation_id"]
        .into_iter()
        .find_map(|key| event.get(key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid_input("native coding session id is required"))
}

fn new_stop_invocation_id() -> io::Result<String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|err| invalid_input(format!("generate Stop hook identity failed: {err}")))?;
    Ok(format!(
        "stop-random:{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn missing_stop_feedback(response: &Value) -> String {
    let artifacts = string_array(response.get("missing_artifacts"));
    let effects = string_array(response.get("missing_effects"));
    let mut missing = Vec::new();
    if !artifacts.is_empty() {
        missing.push(format!("artifacts: {}", artifacts.join(", ")));
    }
    if !effects.is_empty() {
        missing.push(format!("effects: {}", effects.join(", ")));
    }
    if missing.is_empty() {
        "Required outputs are not complete.".to_string()
    } else {
        format!("Missing required outputs: {}.", missing.join("; "))
    }
}

fn string_array(value: Option<&Value>) -> Vec<&str> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn platform_for_source(source: &str) -> io::Result<&'static str> {
    if source.starts_with("codex_") {
        Ok("codex")
    } else if source.starts_with("claude_") {
        Ok("claude")
    } else {
        Err(invalid_input("unsupported participant platform source"))
    }
}

fn validate_current_pane(binding: &ParticipantBindingFile) -> io::Result<()> {
    let pane = std::env::var("TMUX_PANE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid_input("TMUX_PANE is required for participant hooks"))?;
    if pane != binding.pane_id {
        return Err(invalid_input(
            "participant binding does not match the current tmux pane",
        ));
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
