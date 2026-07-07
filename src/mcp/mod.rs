use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Write};

use crate::adapters::tmux::{CommandRunner, SystemCommandRunner, TmuxAdapter};
use crate::flow;
use crate::runtime::{self, BoardPatch, NodeSpec, Runtime, StopContract};
use crate::view::{VisualizationSnapshot, render_terminal_dashboard, serve_browser_snapshot};
use flow_json::flow_draft_json;
use serde_json::{Value, json};

mod flow_json;
mod route_preview;
mod surface;

pub use surface::{AUTHORING_TOOL_NAMES, McpSurface, McpToolDescriptor, RUNTIME_TOOL_NAMES};

pub struct McpServer<R: CommandRunner = SystemCommandRunner> {
    surface: McpSurface,
    state: McpServerState,
    tmux_adapter: TmuxAdapter<R>,
}

impl McpServer<SystemCommandRunner> {
    pub fn new() -> Self {
        Self::with_tmux_runner(SystemCommandRunner)
    }
}

impl Default for McpServer<SystemCommandRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: CommandRunner> McpServer<R> {
    pub fn with_tmux_runner(runner: R) -> Self {
        Self {
            surface: McpSurface,
            state: McpServerState::default(),
            tmux_adapter: TmuxAdapter::with_runner(runner),
        }
    }

    pub fn handle_json_rpc(&mut self, request: Value) -> Option<Value> {
        let id = request.get("id").cloned();
        let method = match request.get("method").and_then(Value::as_str) {
            Some(method) => method,
            None => return Some(error_response(id, -32600, "invalid JSON-RPC request")),
        };

        id.as_ref()?;

        match method {
            "initialize" => Some(success_response(id, initialize_result())),
            "tools/list" => Some(success_response(id, self.surface.tools_list_json())),
            "tools/call" => Some(self.handle_tool_call(id, request.get("params"))),
            _ => Some(error_response(id, -32601, "method not found")),
        }
    }

    fn handle_tool_call(&mut self, id: Option<Value>, params: Option<&Value>) -> Value {
        let Some(params) = params.and_then(Value::as_object) else {
            return error_response(id, -32602, "tools/call params must be an object");
        };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error_response(id, -32602, "tools/call params.name must be a string");
        };

        if self.surface.lookup(name).is_none() {
            return error_response(id, -32602, "unknown tool");
        }

        let Some(arguments) = params.get("arguments") else {
            return error_response(id, -32602, "tools/call params.arguments must be an object");
        };
        if !arguments.is_object() {
            return error_response(id, -32602, "tools/call params.arguments must be an object");
        }

        match self.call_tool(name, arguments) {
            Ok(tool_result) => success_response(id, tool_result.to_json()),
            Err(err) => error_response(id, -32602, &err.message),
        }
    }

    fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        match name {
            "start_run" => self.start_run(arguments),
            "get_context" => self.get_context(arguments),
            "deliver_artifact" => self.deliver_artifact(arguments),
            "fanout_from_artifact" => self.fanout_from_artifact(arguments),
            "record_effect" => self.record_effect(arguments),
            "patch_board" => self.patch_board(arguments),
            "activate_node" => self.activate_node(arguments),
            "send_message" => self.send_message(arguments),
            "validate_stop" => self.validate_stop(arguments),
            "apply_flow_lock" => self.apply_flow_lock(arguments),
            "preview_flow_routes" => self.preview_flow_routes(arguments),
            "view_terminal" => self.view_terminal(arguments),
            "view_snapshot" => self.view_snapshot(arguments),
            "view_browser" => self.view_browser(arguments),
            "flow_apply" => self.flow_apply(arguments),
            "flow_suggest" => self.flow_suggest(arguments),
            "flow_check" => self.flow_check(arguments),
            "flow_lock" => self.flow_lock(arguments),
            "flow_export" => self.flow_export(arguments),
            _ => Ok(ToolCallResult::error(
                json!({ "ok": false, "error": "unknown tool" }),
            )),
        }
    }

    fn start_run(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let nodes = node_specs(arguments)?;
        validate_start_run_preconditions(&self.state.runtime, run_id, &nodes)?;
        let tmux = self.start_run_tmux_metadata(run_id, arguments)?;
        let activation_ids = self
            .state
            .runtime
            .start_run(run_id, nodes)
            .map_err(ToolError::from_runtime)?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "activation_ids": activation_ids,
            "tmux": tmux
        })))
    }

    fn start_run_tmux_metadata(&self, run_id: &str, arguments: &Value) -> Result<Value, ToolError> {
        match tmux_start_options(arguments)? {
            TmuxStartOptions::Disabled => Ok(json!({
                "enabled": false,
                "created": false
            })),
            TmuxStartOptions::Enabled {
                session_id,
                window_name,
            } => {
                let session = self
                    .tmux_adapter
                    .ensure_session(session_id.as_str())
                    .map_err(ToolError::from_tmux)?;
                let window = self
                    .tmux_adapter
                    .create_window_named(&session, run_id, window_name.as_str())
                    .map_err(ToolError::from_tmux)?;

                Ok(json!({
                    "enabled": true,
                    "created": true,
                    "session_id": session.id(),
                    "window_id": window.id(),
                    "window_name": window.name(),
                    "run_id": window.run_id()
                }))
            }
        }
    }

    fn get_context(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        match optional_string(arguments, &["run_id", "runId"])? {
            Some(run_id) => {
                if !self.state.runtime.has_run(run_id) {
                    return Err(ToolError::from_runtime(
                        runtime::RuntimeError::RunNotFound {
                            run_id: run_id.to_owned(),
                        },
                    ));
                }
                let snapshot = self.state.runtime_snapshot();
                let context = snapshot
                    .run(run_id)
                    .expect("checked run should be present in view snapshot")
                    .to_context_json();
                Ok(ToolCallResult::ok(
                    json!({ "ok": true, "run_id": run_id, "context": context }),
                ))
            }
            None => {
                let snapshot = self.state.runtime_snapshot();
                let runs = snapshot
                    .runs
                    .iter()
                    .map(|run| run.to_context_json())
                    .collect::<Vec<_>>();
                Ok(ToolCallResult::ok(json!({ "ok": true, "runs": runs })))
            }
        }
    }

    fn deliver_artifact(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        require_string_arguments(
            arguments,
            &[
                &["run_id", "runId"],
                &["activation_id", "activationId"],
                &["artifact_key", "artifactKey", "key"],
            ],
        )?;
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let activation_id = require_string(arguments, &["activation_id", "activationId"])?;
        let artifact_key = require_string(arguments, &["artifact_key", "artifactKey", "key"])?;
        let payload = payload_string(arguments.get("payload"))?;
        let artifact_id = self
            .state
            .runtime
            .deliver_artifact(run_id, activation_id, artifact_key, payload)
            .map_err(ToolError::from_runtime)?;
        let record = self
            .state
            .runtime
            .state()
            .artifact_records
            .get(&artifact_id)
            .cloned();

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "activation_id": activation_id,
            "artifact_key": artifact_key,
            "artifact_id": artifact_id,
            "content_hash": record.as_ref().map(|artifact| artifact.content_hash.as_str())
        })))
    }

    fn fanout_from_artifact(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        require_string_arguments(
            arguments,
            &[
                &["run_id", "runId"],
                &["node_id", "nodeId"],
                &["artifact_key", "artifactKey", "key"],
            ],
        )?;
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let node_id = require_string(arguments, &["node_id", "nodeId"])?;
        let artifact_key = require_string(arguments, &["artifact_key", "artifactKey", "key"])?;
        let node = node_spec_from_arguments(node_id, arguments)?;
        let activation_ids = self
            .state
            .runtime
            .fanout_from_artifact(run_id, &node, artifact_key)
            .map_err(ToolError::from_runtime)?;
        let state = self.state.runtime.state();
        let activations = activation_ids
            .iter()
            .map(|activation_id| {
                let stable_key = state
                    .activations
                    .get(&(run_id.to_string(), activation_id.clone()))
                    .and_then(|activation| activation.stable_key.clone());
                json!({
                    "activation_id": activation_id,
                    "stable_key": stable_key
                })
            })
            .collect::<Vec<_>>();

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "node_id": node_id,
            "artifact_key": artifact_key,
            "activation_count": activation_ids.len(),
            "activation_ids": activation_ids,
            "activations": activations
        })))
    }

    fn record_effect(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let activation_id = require_string(arguments, &["activation_id", "activationId"])?;
        let effect_key = require_string(arguments, &["effect_key", "effectKey", "key"])?;
        let payload = payload_string(arguments.get("payload"))?;
        self.state
            .runtime
            .record_effect(run_id, activation_id, effect_key, payload)
            .map_err(ToolError::from_runtime)?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "activation_id": activation_id,
            "effect_key": effect_key
        })))
    }

    fn patch_board(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let activation_id = require_string(arguments, &["activation_id", "activationId"])?;
        let patch = require_object_arg(arguments, &["patch"])?;
        if patch.is_empty() {
            return Err(ToolError::invalid("patch must include at least one key"));
        }
        let expected_version = optional_u64(arguments, &["expected_version", "expectedVersion"])?;
        let mut board_version = self.state.runtime.state().board_version;
        for (index, (key, value)) in patch.iter().enumerate() {
            let mut board_patch = BoardPatch::new(key, value_as_string(value)?);
            if index == 0 {
                if let Some(expected_version) = expected_version {
                    board_patch = board_patch.expect_version(expected_version);
                }
            }
            board_version = self
                .state
                .runtime
                .patch_board(run_id, activation_id, board_patch)
                .map_err(ToolError::from_runtime)?;
        }

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "activation_id": activation_id,
            "board_version": board_version
        })))
    }

    fn activate_node(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let node_id = require_string(arguments, &["node_id", "nodeId"])?;
        let requested_activation_id =
            optional_string(arguments, &["activation_id", "activationId"])?;
        if let Some(requested_activation_id) = requested_activation_id {
            if requested_activation_id != node_id {
                return Err(ToolError::invalid(
                    "activation_id must match node_id when using the public runtime API",
                ));
            }
        }
        let node = node_spec_from_arguments(node_id, arguments)?;
        let activation_id = self
            .state
            .runtime
            .activate_node(run_id, &node, None)
            .map_err(ToolError::from_runtime)?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "node_id": node_id,
            "activation_id": activation_id
        })))
    }

    fn send_message(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let message = arguments
            .get("message")
            .ok_or_else(|| ToolError::missing("message"))?
            .clone();
        let message_count = {
            let messages = self.state.messages.entry(run_id.to_string()).or_default();
            messages.push(message);
            messages.len()
        };

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "message_count": message_count
        })))
    }

    fn validate_stop(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let activation_id = require_string(arguments, &["activation_id", "activationId"])?;

        match self.state.runtime.validate_stop(run_id, activation_id) {
            Ok(()) => Ok(ToolCallResult::ok(json!({
                "ok": true,
                "run_id": run_id,
                "activation_id": activation_id,
                "valid": true,
                "missing": []
            }))),
            Err(err) => Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "activation_id": activation_id,
                "valid": false,
                "missing": stop_validation_missing(&err),
                "error": err.to_string()
            }))),
        }
    }

    fn apply_flow_lock(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let mode = flow_lock_mode_arg(arguments)?;
        let lock_id = require_string(
            arguments,
            &["lock_id", "lockId", "flow_lock_id", "flowLockId"],
        )?;
        let provided_content_hash = require_string(arguments, &["content_hash", "contentHash"])?;
        let Some(lock) = self.state.flow_locks.get(lock_id) else {
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "lock_id": lock_id,
                "flow_lock_id": lock_id,
                "error": "flow lock not found"
            })));
        };
        let expected_content_hash = content_hash(lock.normalized_content());
        if provided_content_hash != expected_content_hash {
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "mode": flow_lock_mode_name(mode),
                "lock_id": lock_id,
                "flow_lock_id": lock_id,
                "content_hash": provided_content_hash,
                "expected_content_hash": expected_content_hash,
                "error": "flow lock content hash mismatch"
            })));
        }
        if !self.state.runtime.has_run(run_id) {
            let mut structured = run_not_found_guidance(run_id);
            if let Some(object) = structured.as_object_mut() {
                object.insert("mode".to_string(), json!(flow_lock_mode_name(mode)));
                object.insert("lock_id".to_string(), json!(lock_id));
                object.insert("flow_lock_id".to_string(), json!(lock_id));
                object.insert("content_hash".to_string(), json!(provided_content_hash));
            }
            return Ok(ToolCallResult::error(structured));
        }

        self.state
            .runtime
            .apply_flow_lock(run_id, mode, lock_id, provided_content_hash)
            .map_err(ToolError::from_runtime)?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "mode": flow_lock_mode_name(mode),
            "lock_id": lock_id,
            "content_hash": provided_content_hash
        })))
    }

    fn preview_flow_routes(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        route_preview::preview_flow_routes(&self.state, arguments)
    }

    fn view_terminal(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let snapshot = self.view_snapshot_arg(arguments)?;
        let run_count = snapshot.runs.len();
        let dashboard = render_terminal_dashboard(&snapshot);

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "format": "terminal",
            "dashboard": dashboard,
            "run_count": run_count
        })))
    }

    fn view_snapshot(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let snapshot = self.view_snapshot_arg(arguments)?;
        let run_count = snapshot.runs.len();

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "format": "json",
            "snapshot": snapshot,
            "run_count": run_count
        })))
    }

    fn view_browser(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let host = optional_string(arguments, &["host"])?.unwrap_or("127.0.0.1");
        let host = match host {
            "127.0.0.1" | "localhost" => "127.0.0.1",
            _ => {
                return Err(ToolError::invalid(
                    "view_browser host must be loopback: 127.0.0.1 or localhost",
                ));
            }
        };
        let port = optional_u64(arguments, &["port"])?
            .map(u16::try_from)
            .transpose()
            .map_err(|_| ToolError::invalid("port must be between 0 and 65535"))?
            .unwrap_or(0);
        let snapshot = self.state.runtime_snapshot();
        let run_count = snapshot.runs.len();
        let server = serve_browser_snapshot(host, port, &snapshot).map_err(ToolError::from_view)?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "url": server.url,
            "host": server.host,
            "port": server.port,
            "run_count": run_count
        })))
    }

    fn view_snapshot_arg(&self, arguments: &Value) -> Result<VisualizationSnapshot, ToolError> {
        let snapshot = self.state.runtime_snapshot();
        match optional_string(arguments, &["run_id", "runId"])? {
            Some(run_id) => {
                let Some(run) = snapshot.run(run_id) else {
                    return Err(ToolError::from_runtime(
                        runtime::RuntimeError::RunNotFound {
                            run_id: run_id.to_string(),
                        },
                    ));
                };
                Ok(VisualizationSnapshot {
                    runs: vec![run.clone()],
                })
            }
            None => Ok(snapshot),
        }
    }

    fn flow_apply(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
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
                    let content_hash = content_hash(lock.normalized_content());
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
        let content_hash = content_hash(lock.normalized_content());

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "mode": flow_check_mode_name(lock.mode()),
            "flow_lock_id": flow_lock_id,
            "lock_id": flow_lock_id,
            "content_hash": content_hash,
            "diagnostics": diagnostics_json(lock.diagnostics())
        })))
    }

    fn flow_suggest(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
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

    fn flow_check(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
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

    fn flow_lock(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let draft = flow_draft_arg(arguments)?;
        let mode = flow_check_mode_arg(arguments)?;
        match flow::flow_lock(&draft, mode) {
            Ok(lock) => {
                let lock_id = lock.id().to_string();
                let content_hash = content_hash(lock.normalized_content());
                self.state.flow_locks.insert(lock_id.clone(), lock);
                Ok(ToolCallResult::ok(json!({
                    "ok": true,
                    "mode": flow_check_mode_name(mode),
                    "flow_lock_id": lock_id,
                    "lock_id": lock_id,
                    "content_hash": content_hash
                })))
            }
            Err(err) => Ok(ToolCallResult::error(json!({
                "ok": false,
                "mode": flow_check_mode_name(mode),
                "diagnostics": diagnostics_json(&err.diagnostics)
            }))),
        }
    }

    fn flow_export(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let flow_lock_id = require_string(
            arguments,
            &["flow_lock_id", "flowLockId", "lock_id", "lockId"],
        )?;
        let format = flow_export_format_arg(arguments)?;
        let Some(lock) = self.state.flow_locks.get(flow_lock_id) else {
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

pub fn serve_stdio<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<()> {
    let mut server = McpServer::new();

    while let Some(message) = read_wire_message(reader)? {
        let request = match serde_json::from_str::<Value>(&message.body) {
            Ok(request) => request,
            Err(_) => {
                write_wire_message(
                    writer,
                    message.format,
                    &error_response(None, -32700, "parse error"),
                )?;
                continue;
            }
        };

        if let Some(response) = server.handle_json_rpc(request) {
            write_wire_message(writer, message.format, &response)?;
        }
    }

    Ok(())
}

#[derive(Debug, Default)]
struct McpServerState {
    runtime: Runtime,
    flow_locks: BTreeMap<String, flow::FlowLock>,
    messages: BTreeMap<String, Vec<Value>>,
}

impl McpServerState {
    fn runtime_snapshot(&self) -> VisualizationSnapshot {
        VisualizationSnapshot::from_runtime(self.runtime.state(), &self.message_counts())
    }

    fn message_counts(&self) -> BTreeMap<String, usize> {
        self.messages
            .iter()
            .map(|(run_id, messages)| (run_id.clone(), messages.len()))
            .collect()
    }
}

#[derive(Debug, Clone)]
struct ToolCallResult {
    structured: Value,
    is_error: bool,
}

impl ToolCallResult {
    fn ok(structured: Value) -> Self {
        Self {
            structured,
            is_error: false,
        }
    }

    fn error(structured: Value) -> Self {
        Self {
            structured,
            is_error: true,
        }
    }

    fn to_json(&self) -> Value {
        let text = serde_json::to_string(&self.structured)
            .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"serialization failed\"}".to_string());

        json!({
            "content": [
                {
                    "type": "text",
                    "text": text
                }
            ],
            "structuredContent": self.structured,
            "isError": self.is_error
        })
    }
}

fn run_not_found_guidance(run_id: &str) -> Value {
    json!({
        "ok": false,
        "run_id": run_id,
        "error": "run not found",
        "next_tool": "start_run",
        "next_arguments": {
            "run_id": run_id,
            "nodes": ["root"]
        }
    })
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum WireFormat {
    Line,
    ContentLength,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct WireMessage {
    format: WireFormat,
    body: String,
}

fn read_wire_message<R: BufRead>(reader: &mut R) -> io::Result<Option<WireMessage>> {
    loop {
        let mut first_line = String::new();
        if reader.read_line(&mut first_line)? == 0 {
            return Ok(None);
        }

        if first_line.trim().is_empty() {
            continue;
        }

        if let Some(length) = content_length(&first_line) {
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header)? == 0 {
                    return Ok(None);
                }
                if header.trim().is_empty() {
                    break;
                }
            }

            let mut body = vec![0; length];
            reader.read_exact(&mut body)?;
            let body = String::from_utf8(body)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

            return Ok(Some(WireMessage {
                format: WireFormat::ContentLength,
                body,
            }));
        }

        return Ok(Some(WireMessage {
            format: WireFormat::Line,
            body: first_line.trim_end_matches(['\r', '\n']).to_string(),
        }));
    }
}

fn write_wire_message<W: Write>(
    writer: &mut W,
    format: WireFormat,
    response: &Value,
) -> io::Result<()> {
    let body = response.to_string();
    match format {
        WireFormat::Line => {
            writeln!(writer, "{body}")?;
        }
        WireFormat::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n{body}", body.len())?;
        }
    }
    writer.flush()
}

fn content_length(line: &str) -> Option<usize> {
    let (name, value) = line.split_once(':')?;
    if name.trim().eq_ignore_ascii_case("content-length") {
        value.trim().parse().ok()
    } else {
        None
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "humanize-plugin-mcp",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn success_response(id: Option<Value>, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result
    })
}

fn error_response(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {
            "code": code,
            "message": message
        }
    })
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ToolError {
    message: String,
}

impl ToolError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn missing(name: &str) -> Self {
        Self::invalid(format!("missing required argument: {name}"))
    }

    fn from_runtime(err: runtime::RuntimeError) -> Self {
        Self::invalid(err.to_string())
    }

    fn from_tmux(err: crate::adapters::tmux::TmuxError) -> Self {
        Self::invalid(format!("tmux {err}"))
    }

    fn from_view(err: io::Error) -> Self {
        Self::invalid(format!("view browser {err}"))
    }
}

fn require_string<'a>(arguments: &'a Value, names: &[&str]) -> Result<&'a str, ToolError> {
    for name in names {
        if let Some(value) = arguments.get(*name) {
            return value
                .as_str()
                .ok_or_else(|| ToolError::invalid(format!("{name} must be a string")));
        }
    }
    Err(ToolError::missing(names[0]))
}

fn optional_string<'a>(arguments: &'a Value, names: &[&str]) -> Result<Option<&'a str>, ToolError> {
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

fn require_string_arguments(arguments: &Value, fields: &[&[&str]]) -> Result<(), ToolError> {
    let mut missing = Vec::new();
    for names in fields {
        let mut found = false;
        for name in *names {
            if let Some(value) = arguments.get(*name) {
                found = true;
                if !value.is_string() {
                    return Err(ToolError::invalid(format!("{name} must be a string")));
                }
            }
        }
        if !found {
            missing.push(names[0]);
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(ToolError::invalid(format!(
            "missing required arguments: {}",
            missing.join(", ")
        )))
    }
}

fn optional_u64(arguments: &Value, names: &[&str]) -> Result<Option<u64>, ToolError> {
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

fn require_object_arg<'a>(
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

fn payload_string(value: Option<&Value>) -> Result<String, ToolError> {
    match value {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(value) => serde_json::to_string(value)
            .map_err(|_| ToolError::invalid("payload must be JSON serializable")),
        None => Ok("null".to_string()),
    }
}

fn value_as_string(value: &Value) -> Result<String, ToolError> {
    match value {
        Value::String(value) => Ok(value.clone()),
        value => serde_json::to_string(value)
            .map_err(|_| ToolError::invalid("value must be JSON serializable")),
    }
}

fn node_specs(arguments: &Value) -> Result<Vec<NodeSpec>, ToolError> {
    let nodes = match arguments.get("nodes") {
        Some(value) => value
            .as_array()
            .ok_or_else(|| ToolError::invalid("nodes must be an array"))?,
        None => {
            return Ok(vec![node_spec_from_arguments("root", arguments)?]);
        }
    };

    if nodes.is_empty() {
        return Ok(vec![node_spec_from_arguments("root", arguments)?]);
    }

    nodes
        .iter()
        .map(|node| match node {
            Value::String(id) => Ok(NodeSpec::new(id)),
            Value::Object(object) => node_spec_from_object(object),
            _ => Err(ToolError::invalid("nodes items must be strings or objects")),
        })
        .collect()
}

fn validate_start_run_preconditions(
    runtime: &Runtime,
    run_id: &str,
    nodes: &[NodeSpec],
) -> Result<(), ToolError> {
    if runtime.has_run(run_id) {
        return Err(ToolError::from_runtime(
            runtime::RuntimeError::DuplicateRun {
                run_id: run_id.to_string(),
            },
        ));
    }

    let mut seen_activation_ids = BTreeSet::new();
    for node in nodes {
        let activation_id = node.id().to_string();
        if !seen_activation_ids.insert(activation_id.clone()) {
            return Err(ToolError::from_runtime(
                runtime::RuntimeError::DuplicateActivation { activation_id },
            ));
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum TmuxStartOptions {
    Disabled,
    Enabled {
        session_id: String,
        window_name: String,
    },
}

fn tmux_start_options(arguments: &Value) -> Result<TmuxStartOptions, ToolError> {
    let Some(value) = arguments.get("tmux") else {
        return Ok(TmuxStartOptions::Disabled);
    };
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("tmux must be an object"))?;
    let enabled = match object.get("enabled") {
        Some(Value::Bool(enabled)) => *enabled,
        Some(_) => return Err(ToolError::invalid("tmux.enabled must be a boolean")),
        None => false,
    };

    if !enabled {
        return Ok(TmuxStartOptions::Disabled);
    }

    Ok(TmuxStartOptions::Enabled {
        session_id: string_field(object, &["session", "session_id", "sessionId"])?.to_string(),
        window_name: string_field(object, &["window", "window_name", "windowName"])?.to_string(),
    })
}

fn node_spec_from_arguments(id: &str, arguments: &Value) -> Result<NodeSpec, ToolError> {
    let required_artifacts =
        optional_string_array(arguments, &["required_artifacts", "requiredArtifacts"])?;
    let required_effects =
        optional_string_array(arguments, &["required_effects", "requiredEffects"])?;
    let mut node = NodeSpec::new(id)
        .with_stop_contract(StopContract::new(required_artifacts, required_effects));
    if let Some(for_each) = optional_string(arguments, &["for_each", "forEach"])? {
        node = node.with_for_each(for_each);
    }
    Ok(node)
}

fn node_spec_from_object(object: &serde_json::Map<String, Value>) -> Result<NodeSpec, ToolError> {
    let id = string_field(object, &["id", "node_id", "nodeId"])?;
    let required_artifacts =
        optional_string_array_from_object(object, &["required_artifacts", "requiredArtifacts"])?;
    let required_effects =
        optional_string_array_from_object(object, &["required_effects", "requiredEffects"])?;
    let mut node = NodeSpec::new(id)
        .with_stop_contract(StopContract::new(required_artifacts, required_effects));
    if let Some(for_each) = optional_string_field(object, &["for_each", "forEach"])? {
        node = node.with_for_each(for_each);
    }
    Ok(node)
}

fn string_field<'a>(
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

fn optional_string_field<'a>(
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

fn optional_string_array(arguments: &Value, names: &[&str]) -> Result<Vec<String>, ToolError> {
    for name in names {
        if let Some(value) = arguments.get(*name) {
            return string_array(value, name);
        }
    }
    Ok(Vec::new())
}

fn optional_string_array_from_object(
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

fn string_array(value: &Value, name: &str) -> Result<Vec<String>, ToolError> {
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

fn flow_lock_mode_arg(arguments: &Value) -> Result<runtime::FlowLockMode, ToolError> {
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

fn flow_lock_mode_name(mode: runtime::FlowLockMode) -> &'static str {
    match mode {
        runtime::FlowLockMode::FutureActivations => "future_activations",
        runtime::FlowLockMode::CheckpointRestart => "checkpoint_restart",
    }
}

fn stop_validation_missing(err: &runtime::StopValidationError) -> Vec<String> {
    match err {
        runtime::StopValidationError::RunNotFound { .. }
        | runtime::StopValidationError::ActivationNotFound { .. }
        | runtime::StopValidationError::ActivationNotFoundInRun { .. } => {
            vec!["activation".into()]
        }
        runtime::StopValidationError::MissingArtifact { artifact_key, .. } => {
            vec![format!("artifact:{artifact_key}")]
        }
        runtime::StopValidationError::MissingEffect { effect_key, .. } => {
            vec![format!("effect:{effect_key}")]
        }
    }
}

fn flow_suggest_input_arg(arguments: &Value) -> Result<flow::FlowSuggestInput, ToolError> {
    Ok(flow::FlowSuggestInput {
        goal: require_string(arguments, &["goal"])?.to_string(),
        nodes: optional_string_array(arguments, &["nodes"])?,
        artifact: optional_string(arguments, &["artifact"])?.map(str::to_string),
    })
}

fn flow_draft_arg(arguments: &Value) -> Result<flow::FlowDraft, ToolError> {
    let flow = require_object_arg(arguments, &["flow"])?;

    Ok(flow::FlowDraft {
        nodes: optional_array_field(flow, "nodes")?
            .iter()
            .map(parse_flow_node)
            .collect::<Result<Vec<_>, _>>()?,
        contracts: optional_array_field(flow, "contracts")?
            .iter()
            .map(parse_flow_contract)
            .collect::<Result<Vec<_>, _>>()?,
        routes: optional_array_field(flow, "routes")?
            .iter()
            .map(parse_flow_route)
            .collect::<Result<Vec<_>, _>>()?,
        resources: optional_array_field(flow, "resources")?
            .iter()
            .map(parse_flow_resource)
            .collect::<Result<Vec<_>, _>>()?,
        imports: optional_array_field(flow, "imports")?
            .iter()
            .map(parse_flow_import)
            .collect::<Result<Vec<_>, _>>()?,
        policies: parse_flow_policies(flow.get("policies"))?,
        extensions: match flow.get("extensions") {
            Some(value) => string_array(value, "extensions")?,
            None => Vec::new(),
        },
    })
}

fn flow_draft_is_empty(draft: &flow::FlowDraft) -> bool {
    draft.nodes.is_empty()
        && draft.contracts.is_empty()
        && draft.routes.is_empty()
        && draft.resources.is_empty()
        && draft.imports.is_empty()
        && draft.policies == flow::FlowPolicies::default()
        && draft.extensions.is_empty()
}

fn optional_array_field<'a>(
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

fn parse_flow_node(value: &Value) -> Result<flow::FlowNode, ToolError> {
    match value {
        Value::String(id) => Ok(flow::FlowNode {
            id: id.clone(),
            ..flow::FlowNode::default()
        }),
        Value::Object(object) => Ok(flow::FlowNode {
            id: string_field(object, &["id"])?.to_string(),
            contract_id: optional_string_field(object, &["contract_id", "contractId"])?
                .map(str::to_string),
            action: object.get("action").map(parse_node_action).transpose()?,
            write_scopes: match object
                .get("write_scopes")
                .or_else(|| object.get("writeScopes"))
            {
                Some(value) => parse_write_scopes(value, "write_scopes")?,
                None => Vec::new(),
            },
            extensions: match object.get("extensions") {
                Some(value) => string_array(value, "extensions")?,
                None => Vec::new(),
            },
        }),
        _ => Err(ToolError::invalid("nodes items must be strings or objects")),
    }
}

fn parse_node_action(value: &Value) -> Result<flow::NodeAction, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("action must be an object"))?;
    Ok(flow::NodeAction {
        driver: parse_node_driver(string_field(object, &["driver"])?)?,
        prompt_ref: optional_string_field(object, &["prompt_ref", "promptRef"])?
            .map(str::to_string),
        resource_refs: optional_string_array_from_object(
            object,
            &["resource_refs", "resourceRefs"],
        )?,
        reads: match object.get("reads") {
            Some(value) => string_array(value, "reads")?,
            None => Vec::new(),
        },
        writes: match object.get("writes") {
            Some(value) => string_array(value, "writes")?,
            None => Vec::new(),
        },
        verdict_artifact: optional_string_field(object, &["verdict_artifact", "verdictArtifact"])?
            .map(str::to_string),
    })
}

fn parse_node_driver(value: &str) -> Result<flow::NodeDriver, ToolError> {
    match value {
        "agent" | "Agent" => Ok(flow::NodeDriver::Agent),
        "script" | "Script" => Ok(flow::NodeDriver::Script),
        "review" | "Review" => Ok(flow::NodeDriver::Review),
        "human" | "Human" => Ok(flow::NodeDriver::Human),
        value => Err(ToolError::invalid(format!(
            "unknown action driver: {value}"
        ))),
    }
}

fn parse_flow_contract(value: &Value) -> Result<flow::FlowContract, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("contracts items must be objects"))?;
    Ok(flow::FlowContract {
        id: string_field(object, &["id"])?.to_string(),
        completion: optional_string_field(object, &["completion"])?
            .map(parse_contract_completion)
            .transpose()?,
        artifacts: optional_array_field(object, "artifacts")?
            .iter()
            .map(parse_contract_artifact)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn parse_contract_completion(value: &str) -> Result<flow::ContractCompletion, ToolError> {
    match value {
        "manual" | "Manual" => Ok(flow::ContractCompletion::Manual),
        "all_artifacts" | "allArtifacts" | "AllArtifacts" => {
            Ok(flow::ContractCompletion::AllArtifacts)
        }
        value => Err(ToolError::invalid(format!(
            "unknown contract completion: {value}"
        ))),
    }
}

fn parse_contract_artifact(value: &Value) -> Result<flow::ContractArtifact, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("artifacts items must be objects"))?;
    Ok(flow::ContractArtifact {
        id: string_field(object, &["id"])?.to_string(),
        schema_resource_id: optional_string_field(
            object,
            &["schema_resource_id", "schemaResourceId"],
        )?
        .map(str::to_string),
    })
}

fn parse_flow_route(value: &Value) -> Result<flow::FlowRoute, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("routes items must be objects"))?;
    Ok(flow::FlowRoute {
        predicate: string_field(object, &["predicate"])?.to_string(),
        for_each: optional_string_field(object, &["for_each", "forEach"])?.map(str::to_string),
        activate: string_field(object, &["activate"])?.to_string(),
    })
}

fn parse_flow_resource(value: &Value) -> Result<flow::FlowResource, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("resources items must be objects"))?;
    Ok(flow::FlowResource {
        id: string_field(object, &["id"])?.to_string(),
        kind: parse_resource_kind(string_field(object, &["kind"])?)?,
        source: string_field(object, &["source"])?.to_string(),
    })
}

fn parse_resource_kind(value: &str) -> Result<flow::ResourceKind, ToolError> {
    match value {
        "schema" | "Schema" => Ok(flow::ResourceKind::Schema),
        "rule" | "Rule" => Ok(flow::ResourceKind::Rule),
        "profile" | "Profile" => Ok(flow::ResourceKind::Profile),
        "view" | "View" => Ok(flow::ResourceKind::View),
        "prompt" | "Prompt" => Ok(flow::ResourceKind::Prompt),
        "script" | "Script" => Ok(flow::ResourceKind::Script),
        "flow" | "Flow" => Ok(flow::ResourceKind::Flow),
        "readme" | "Readme" | "README" => Ok(flow::ResourceKind::Readme),
        value => Err(ToolError::invalid(format!(
            "unknown resource kind: {value}"
        ))),
    }
}

fn parse_flow_import(value: &Value) -> Result<flow::FlowImport, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("imports items must be objects"))?;
    Ok(flow::FlowImport {
        resource_id: string_field(object, &["resource_id", "resourceId"])?.to_string(),
        alias: optional_string_field(object, &["alias"])?.map(str::to_string),
    })
}

fn parse_flow_policies(value: Option<&Value>) -> Result<flow::FlowPolicies, ToolError> {
    let Some(value) = value else {
        return Ok(flow::FlowPolicies::default());
    };
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("policies must be an object"))?;
    let write_scopes = match object
        .get("write_scopes")
        .or_else(|| object.get("writeScopes"))
    {
        Some(value) => parse_write_scopes(value, "write_scopes")?,
        None => Vec::new(),
    };
    Ok(flow::FlowPolicies { write_scopes })
}

fn parse_write_scopes(value: &Value, name: &str) -> Result<Vec<flow::WriteScope>, ToolError> {
    let scopes = value
        .as_array()
        .ok_or_else(|| ToolError::invalid(format!("{name} must be an array")))?;
    scopes.iter().map(parse_write_scope).collect()
}

fn parse_write_scope(value: &Value) -> Result<flow::WriteScope, ToolError> {
    match value {
        Value::String(value) if value == "workspace" => Ok(flow::WriteScope::Workspace),
        Value::String(value) if value == "system" => Ok(flow::WriteScope::System),
        Value::String(value) if value.starts_with("artifact:") => Ok(flow::WriteScope::Artifact(
            value.trim_start_matches("artifact:").to_string(),
        )),
        Value::String(value) if value.starts_with("resource:") => Ok(flow::WriteScope::Resource(
            value.trim_start_matches("resource:").to_string(),
        )),
        Value::Object(object) => {
            let kind = string_field(object, &["kind", "type"])?;
            match kind {
                "artifact" => Ok(flow::WriteScope::Artifact(
                    string_field(object, &["value", "id"])?.to_string(),
                )),
                "resource" => Ok(flow::WriteScope::Resource(
                    string_field(object, &["value", "id"])?.to_string(),
                )),
                "workspace" => Ok(flow::WriteScope::Workspace),
                "system" => Ok(flow::WriteScope::System),
                value => Err(ToolError::invalid(format!("unknown write scope: {value}"))),
            }
        }
        _ => Err(ToolError::invalid(
            "write scope items must be strings or objects",
        )),
    }
}

fn flow_check_mode_arg(arguments: &Value) -> Result<flow::FlowCheckMode, ToolError> {
    match optional_string(arguments, &["mode"])? {
        Some("core") | None => Ok(flow::FlowCheckMode::Core),
        Some("strict") => Ok(flow::FlowCheckMode::Strict),
        Some(value) => Err(ToolError::invalid(format!(
            "unknown flow check mode: {value}"
        ))),
    }
}

fn flow_check_mode_name(mode: flow::FlowCheckMode) -> &'static str {
    match mode {
        flow::FlowCheckMode::Core => "core",
        flow::FlowCheckMode::Strict => "strict",
    }
}

fn flow_export_format_arg(arguments: &Value) -> Result<flow::FlowExportFormat, ToolError> {
    match optional_string(arguments, &["format"])? {
        Some("json") | None => Ok(flow::FlowExportFormat::Json),
        Some("yaml") => Ok(flow::FlowExportFormat::Yaml),
        Some(value) => Err(ToolError::invalid(format!(
            "unknown flow export format: {value}"
        ))),
    }
}

fn flow_export_format_name(format: flow::FlowExportFormat) -> &'static str {
    match format {
        flow::FlowExportFormat::Json => "json",
        flow::FlowExportFormat::Yaml => "yaml",
    }
}

fn diagnostics_json(diagnostics: &[flow::Diagnostic]) -> Vec<Value> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code,
                "severity": severity_name(diagnostic.severity),
                "location": diagnostic.location,
                "message": diagnostic.message,
                "fix_hint": diagnostic.fix_hint
            })
        })
        .collect()
}

fn severity_name(severity: flow::Severity) -> &'static str {
    match severity {
        flow::Severity::Error => "error",
        flow::Severity::Warning => "warning",
    }
}

fn content_hash(input: &str) -> String {
    format!("fnv1a64:{:016x}", stable_hash(input))
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
