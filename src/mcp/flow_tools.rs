use crate::adapters::tmux::CommandRunner;
use crate::flow;
use serde_json::{Value, json};
use std::path::Path;

use super::flow_json::flow_draft_json;
use super::{
    McpServer, ToolCallResult, ToolError, diagnostics_json, flow_check_mode_arg,
    flow_check_mode_name, flow_draft_arg, flow_draft_is_empty, flow_export_format_arg,
    flow_export_format_name, flow_repair_input_arg, flow_suggest_input_arg, input_severity_name,
    optional_string, repair_candidates_json, repair_guidance_json, require_string,
};

impl<R: CommandRunner> McpServer<R> {
    pub(super) fn flow_repair(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let input = flow_repair_input_arg(arguments)?;
        let report = flow::flow_repair(&input);
        let fatal = report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == flow::Severity::Fatal);
        let repairable = !fatal && !report.candidates.is_empty();
        let structured = json!({
            "ok": true,
            "repairable": repairable,
            "input_severity": input_severity_name(&report.diagnostics),
            "candidates": repair_candidates_json(&report.candidates),
            "guidance": repair_guidance_json(&report.diagnostics),
            "diagnostics": diagnostics_json(&report.diagnostics)
        });

        Ok(ToolCallResult::ok(structured))
    }

    pub(super) fn flow_apply(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        if arguments.get("flow").is_some() {
            let draft = flow_draft_arg(arguments)?;
            if flow_draft_is_empty(&draft) {
                return Err(ToolError::invalid(
                    "flow must include at least one authoring field",
                ));
            }
            let mode = flow_check_mode_arg(arguments)?;
            return match flow::flow_lock(&draft, mode) {
                Ok(lock) => {
                    let lock_id = lock.id().to_string();
                    let content_hash = lock.content_hash().to_string();
                    let diagnostics = diagnostics_json(lock.diagnostics());
                    self.state.flow_locks.insert(lock_id.clone(), lock);
                    Ok(ToolCallResult::ok(json!({
                        "ok": true,
                        "mode": flow_check_mode_name(mode),
                        "flow_lock_id": lock_id,
                        "lock_id": lock_id,
                        "content_hash": content_hash,
                        "diagnostics": diagnostics
                    })))
                }
                Err(err) => Ok(ToolCallResult::error(json!({
                    "ok": false,
                    "mode": flow_check_mode_name(mode),
                    "diagnostics": diagnostics_json(&err.diagnostics)
                }))),
            };
        }

        if arguments.get("flow_lock").is_some()
            || arguments.get("package_path").is_some()
            || arguments.get("packagePath").is_some()
            || arguments.get("flow_lock_path").is_some()
            || arguments.get("flowLockPath").is_some()
        {
            let (lock_id, content_hash) =
                self.require_flow_lock_binding_from_arguments(arguments)?;
            let lock = self
                .state
                .flow_locks
                .get(&lock_id)
                .expect("resolved flow lock should be cached");
            return Ok(ToolCallResult::ok(json!({
                "ok": true,
                "mode": flow_check_mode_name(lock.mode()),
                "flow_lock_id": lock_id,
                "lock_id": lock_id,
                "content_hash": content_hash,
                "flow_lock": lock,
                "diagnostics": diagnostics_json(lock.diagnostics())
            })));
        }

        let flow_lock_id = require_string(
            arguments,
            &["flow_lock_id", "flowLockId", "lock_id", "lockId"],
        )?;
        let Some(lock) = self.state.flow_locks.get(flow_lock_id) else {
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "flow_lock_id": flow_lock_id,
                "error": "flow lock not found"
            })));
        };
        let content_hash = lock.content_hash().to_string();

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "mode": flow_check_mode_name(lock.mode()),
            "flow_lock_id": flow_lock_id,
            "lock_id": flow_lock_id,
            "content_hash": content_hash,
            "diagnostics": diagnostics_json(lock.diagnostics())
        })))
    }

    pub(super) fn flow_suggest(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let input = flow_suggest_input_arg(arguments)?;
        let draft = flow::flow_suggest(input)
            .map_err(|err| ToolError::invalid(err.message().to_string()))?;
        let report = flow::flow_check(&draft, flow::FlowCheckMode::Core);
        let valid = !report.has_errors();
        let structured = json!({
            "ok": valid,
            "flow": flow_draft_json(&draft),
            "mode": flow_check_mode_name(report.mode),
            "diagnostics": diagnostics_json(&report.diagnostics),
            "valid": valid
        });

        if valid {
            Ok(ToolCallResult::ok(structured))
        } else {
            Ok(ToolCallResult::error(structured))
        }
    }

    pub(super) fn flow_check(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let draft = flow_draft_arg(arguments)?;
        let mode = flow_check_mode_arg(arguments)?;
        let report = flow::flow_check(&draft, mode);
        let ok = !report.has_errors();
        let structured = json!({
            "ok": ok,
            "mode": flow_check_mode_name(report.mode),
            "diagnostics": diagnostics_json(&report.diagnostics)
        });

        if ok {
            Ok(ToolCallResult::ok(structured))
        } else {
            Ok(ToolCallResult::error(structured))
        }
    }

    pub(super) fn flow_lock(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let draft = flow_draft_arg(arguments)?;
        let mode = flow_check_mode_arg(arguments)?;
        match flow::flow_lock(&draft, mode) {
            Ok(lock) => {
                let lock_id = lock.id().to_string();
                let content_hash = lock.content_hash().to_string();
                if let Some(package_path) = optional_string(
                    arguments,
                    &[
                        "package_path",
                        "packagePath",
                        "flow_lock_path",
                        "flowLockPath",
                    ],
                )? {
                    lock.write_directory(Path::new(package_path))
                        .map_err(|error| {
                            ToolError::invalid(format!("flow lock package write failed: {error}"))
                        })?;
                }
                let flow_lock = serde_json::to_value(&lock)
                    .map_err(|_| ToolError::invalid("flow lock serialization failed"))?;
                self.state.flow_locks.insert(lock_id.clone(), lock);
                Ok(ToolCallResult::ok(json!({
                    "ok": true,
                    "mode": flow_check_mode_name(mode),
                    "flow_lock_id": lock_id,
                    "lock_id": lock_id,
                    "content_hash": content_hash,
                    "flow_lock": flow_lock,
                    "package_path": optional_string(
                        arguments,
                        &["package_path", "packagePath", "flow_lock_path", "flowLockPath"],
                    )?
                })))
            }
            Err(err) => Ok(ToolCallResult::error(json!({
                "ok": false,
                "mode": flow_check_mode_name(mode),
                "diagnostics": diagnostics_json(&err.diagnostics)
            }))),
        }
    }

    pub(super) fn flow_export(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let (flow_lock_id, _) = self.require_flow_lock_binding_from_arguments(arguments)?;
        let format = flow_export_format_arg(arguments)?;
        let Some(lock) = self.state.flow_locks.get(&flow_lock_id) else {
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "flow_lock_id": flow_lock_id,
                "error": "flow lock not found"
            })));
        };
        let document = flow::flow_export(lock, format);

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "flow_lock_id": flow_lock_id,
            "format": flow_export_format_name(format),
            "document": document
        })))
    }
}
