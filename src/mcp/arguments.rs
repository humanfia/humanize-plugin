use crate::flow;
use crate::runtime::{self, NodeSpec, StopContract};
use serde_json::Value;

use super::ToolError;
use super::flow_parse::flow_draft_for_repair;

pub(super) fn require_string<'a>(
    arguments: &'a Value,
    names: &[&str],
) -> Result<&'a str, ToolError> {
    for name in names {
        if let Some(value) = arguments.get(*name) {
            return value
                .as_str()
                .ok_or_else(|| ToolError::invalid(format!("{name} must be a string")));
        }
    }
    Err(ToolError::missing(names[0]))
}

pub(super) fn optional_string<'a>(
    arguments: &'a Value,
    names: &[&str],
) -> Result<Option<&'a str>, ToolError> {
    for name in names {
        if let Some(value) = arguments.get(*name) {
            return value
                .as_str()
                .map(Some)
                .ok_or_else(|| ToolError::invalid(format!("{name} must be a string")));
        }
    }
    Ok(None)
}

pub(super) fn optional_u64(arguments: &Value, names: &[&str]) -> Result<Option<u64>, ToolError> {
    for name in names {
        if let Some(value) = arguments.get(*name) {
            return value
                .as_u64()
                .map(Some)
                .ok_or_else(|| ToolError::invalid(format!("{name} must be an unsigned integer")));
        }
    }
    Ok(None)
}

pub(super) fn optional_bool(arguments: &Value, names: &[&str]) -> Result<Option<bool>, ToolError> {
    for name in names {
        if let Some(value) = arguments.get(*name) {
            return value
                .as_bool()
                .map(Some)
                .ok_or_else(|| ToolError::invalid(format!("{name} must be a boolean")));
        }
    }
    Ok(None)
}

pub(super) fn require_object_arg<'a>(
    arguments: &'a Value,
    names: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, ToolError> {
    for name in names {
        if let Some(value) = arguments.get(*name) {
            return value
                .as_object()
                .ok_or_else(|| ToolError::invalid(format!("{name} must be an object")));
        }
    }
    Err(ToolError::missing(names[0]))
}

pub(super) fn node_spec_from_arguments(id: &str, arguments: &Value) -> Result<NodeSpec, ToolError> {
    let required_artifacts =
        optional_string_array(arguments, &["required_artifacts", "requiredArtifacts"])?;
    let required_effects =
        optional_string_array(arguments, &["required_effects", "requiredEffects"])?;
    let mut node = NodeSpec::new(id)
        .with_stop_contract(StopContract::new(required_artifacts, required_effects));
    if let Some(for_each) = optional_string(arguments, &["for_each", "forEach"])? {
        node = node.with_for_each(
            flow::ArtifactRef::new(for_each)
                .map_err(|error| ToolError::invalid(error.to_string()))?,
        );
    }
    Ok(node)
}

pub(super) fn string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<&'a str, ToolError> {
    for name in names {
        if let Some(value) = object.get(*name) {
            return value
                .as_str()
                .ok_or_else(|| ToolError::invalid(format!("{name} must be a string")));
        }
    }
    Err(ToolError::missing(names[0]))
}

pub(super) fn optional_string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<Option<&'a str>, ToolError> {
    for name in names {
        if let Some(value) = object.get(*name) {
            return value
                .as_str()
                .map(Some)
                .ok_or_else(|| ToolError::invalid(format!("{name} must be a string")));
        }
    }
    Ok(None)
}

pub(super) fn optional_bool_field(
    object: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<Option<bool>, ToolError> {
    for name in names {
        if let Some(value) = object.get(*name) {
            return value
                .as_bool()
                .map(Some)
                .ok_or_else(|| ToolError::invalid(format!("{name} must be a boolean")));
        }
    }
    Ok(None)
}

pub(super) fn optional_string_array(
    arguments: &Value,
    names: &[&str],
) -> Result<Vec<String>, ToolError> {
    for name in names {
        if let Some(value) = arguments.get(*name) {
            return string_array(value, name);
        }
    }
    Ok(Vec::new())
}

pub(super) fn optional_string_array_from_object(
    object: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<Vec<String>, ToolError> {
    for name in names {
        if let Some(value) = object.get(*name) {
            return string_array(value, name);
        }
    }
    Ok(Vec::new())
}

pub(super) fn string_array(value: &Value, name: &str) -> Result<Vec<String>, ToolError> {
    let values = value
        .as_array()
        .ok_or_else(|| ToolError::invalid(format!("{name} must be an array")))?;
    values
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| ToolError::invalid(format!("{name} items must be strings")))
        })
        .collect()
}

pub(super) fn flow_lock_mode_arg(arguments: &Value) -> Result<runtime::FlowLockMode, ToolError> {
    match require_string(arguments, &["mode"])? {
        "future_activations" | "futureActivations" | "future-activations" => {
            Ok(runtime::FlowLockMode::FutureActivations)
        }
        "checkpoint_restart" | "checkpointRestart" | "checkpoint-restart" => {
            Ok(runtime::FlowLockMode::CheckpointRestart)
        }
        value => Err(ToolError::invalid(format!(
            "unknown flow lock mode: {value}"
        ))),
    }
}

pub(super) fn optional_flow_lock_mode_arg(
    arguments: &Value,
    names: &[&str],
) -> Result<Option<runtime::FlowLockMode>, ToolError> {
    let Some(value) = optional_string(arguments, names)? else {
        return Ok(None);
    };
    match value {
        "future_activations" | "futureActivations" | "future-activations" => {
            Ok(Some(runtime::FlowLockMode::FutureActivations))
        }
        "checkpoint_restart" | "checkpointRestart" | "checkpoint-restart" => {
            Ok(Some(runtime::FlowLockMode::CheckpointRestart))
        }
        value => Err(ToolError::invalid(format!(
            "unknown flow lock mode: {value}"
        ))),
    }
}

pub(super) fn flow_lock_mode_name(mode: runtime::FlowLockMode) -> &'static str {
    match mode {
        runtime::FlowLockMode::FutureActivations => "future_activations",
        runtime::FlowLockMode::CheckpointRestart => "checkpoint_restart",
    }
}

pub(super) fn flow_suggest_input_arg(
    arguments: &Value,
) -> Result<flow::FlowSuggestInput, ToolError> {
    Ok(flow::FlowSuggestInput {
        goal: require_string(arguments, &["goal"])?.to_string(),
        readme: require_string(arguments, &["readme"])?.to_string(),
        nodes: optional_string_array(arguments, &["nodes"])?,
        artifact: optional_string(arguments, &["artifact"])?.map(str::to_string),
    })
}

pub(super) fn flow_repair_input_arg(arguments: &Value) -> Result<flow::FlowRepairInput, ToolError> {
    let mode = flow_check_mode_arg(arguments)?;
    let flow = require_object_arg(arguments, &["flow"])?;
    let draft = flow_draft_for_repair(flow)?;

    Ok(flow::FlowRepairInput {
        draft,
        mode,
        diagnostics: Vec::new(),
        include_warnings: optional_bool(arguments, &["include_warnings", "includeWarnings"])?
            .unwrap_or(false),
    })
}

pub(super) fn optional_array_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Result<&'a [Value], ToolError> {
    match object.get(name) {
        Some(value) => value
            .as_array()
            .map(Vec::as_slice)
            .ok_or_else(|| ToolError::invalid(format!("{name} must be an array"))),
        None => Ok(&[]),
    }
}

pub(super) fn flow_check_mode_arg(arguments: &Value) -> Result<flow::FlowCheckMode, ToolError> {
    match optional_string(arguments, &["mode"])? {
        Some("core") | None => Ok(flow::FlowCheckMode::Core),
        Some("strict") => Ok(flow::FlowCheckMode::Strict),
        Some(value) => Err(ToolError::invalid(format!(
            "unknown flow check mode: {value}"
        ))),
    }
}

pub(super) fn flow_check_mode_name(mode: flow::FlowCheckMode) -> &'static str {
    match mode {
        flow::FlowCheckMode::Core => "core",
        flow::FlowCheckMode::Strict => "strict",
    }
}

pub(super) fn flow_export_format_arg(
    arguments: &Value,
) -> Result<flow::FlowExportFormat, ToolError> {
    match optional_string(arguments, &["format"])? {
        Some("json") | None => Ok(flow::FlowExportFormat::Json),
        Some("yaml") => Ok(flow::FlowExportFormat::Yaml),
        Some(value) => Err(ToolError::invalid(format!(
            "unknown flow export format: {value}"
        ))),
    }
}

pub(super) fn flow_export_format_name(format: flow::FlowExportFormat) -> &'static str {
    match format {
        flow::FlowExportFormat::Json => "json",
        flow::FlowExportFormat::Yaml => "yaml",
    }
}
