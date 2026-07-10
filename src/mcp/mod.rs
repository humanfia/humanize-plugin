use std::collections::{BTreeMap, BTreeSet};
use std::io;

use crate::adapters::tmux::{
    CommandRunner, SystemCommandRunner, TmuxAdapter, TmuxPane, TmuxPipeCapture, TmuxWindow,
};
use crate::flow;
use crate::run_assets::{RunAssetManifest, RunAssetStore};
use crate::runtime::{
    self, BoardPatch, ControlCommand, DriverState, DriverTickInput, NodeSpec, Runtime, StopContract,
};
use crate::view::VisualizationSnapshot;
use serde_json::{Value, json};

mod flow_json;
mod flow_parse;
mod flow_tools;
mod review_tools;
mod route_preview;
mod runtime_control;
mod runtime_snapshot;
mod stdio;
mod surface;
mod tmux_tools;
mod update_tools;
mod view_tools;

pub use stdio::{
    serve_stdio, serve_stdio_signal_aware, serve_stdio_signal_aware_with_server,
    serve_stdio_with_server,
};
pub use surface::{
    AUTHORING_TOOL_NAMES, McpSurface, McpToolDescriptor, REVIEW_TOOL_NAMES, RUNTIME_TOOL_NAMES,
};

use flow_parse::{flow_draft_arg, flow_draft_for_repair, flow_draft_is_empty};

pub struct McpServer<R: CommandRunner = SystemCommandRunner> {
    surface: McpSurface,
    state: McpServerState,
    tmux_adapter: TmuxAdapter<R>,
    run_asset_store: RunAssetStore,
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
        Self::with_tmux_runner_and_run_asset_store(runner, RunAssetStore::runtime_default())
    }

    pub fn with_tmux_runner_and_run_asset_store(runner: R, run_asset_store: RunAssetStore) -> Self {
        Self {
            surface: McpSurface,
            state: McpServerState::default(),
            tmux_adapter: TmuxAdapter::with_runner(runner),
            run_asset_store,
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
            "shutdown" => Some(self.handle_shutdown(id)),
            "tools/list" => Some(success_response(id, self.surface.tools_list_json())),
            "tools/call" => Some(self.handle_tool_call(id, request.get("params"))),
            _ => Some(error_response(id, -32601, "method not found")),
        }
    }

    fn handle_shutdown(&mut self, id: Option<Value>) -> Value {
        let cleanup = self.shutdown_active_tmux_assets("mcp_shutdown");
        self.state.shutdown_requested = true;
        match cleanup {
            Ok(tmux_cleanup) => success_response(
                id,
                json!({
                    "ok": true,
                    "shutdown": true,
                    "tmux_cleanup": tmux_cleanup
                }),
            ),
            Err(err) => success_response(
                id,
                json!({
                    "ok": false,
                    "shutdown": true,
                    "tmux_cleanup": self.state.shutdown_assets_summary,
                    "error": err.message
                }),
            ),
        }
    }

    fn shutdown_requested(&self) -> bool {
        self.state.shutdown_requested
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
            "observe_stop" => self.observe_stop(arguments),
            "apply_flow_lock" => self.apply_flow_lock(arguments),
            "preview_flow_routes" => self.preview_flow_routes(arguments),
            "run_flow" => self.run_flow(arguments),
            "run_status" => self.run_status(arguments),
            "run_why" => self.run_why(arguments),
            "pause_run" => self.pause_run(arguments),
            "resume_run" => self.resume_run(arguments),
            "stop_run" => self.stop_run(arguments),
            "view_terminal" => self.view_terminal(arguments),
            "view_snapshot" => self.view_snapshot(arguments),
            "view_browser" => self.view_browser(arguments),
            "flow_repair" => self.flow_repair(arguments),
            "flow_apply" => self.flow_apply(arguments),
            "flow_suggest" => self.flow_suggest(arguments),
            "flow_check" => self.flow_check(arguments),
            "flow_lock" => self.flow_lock(arguments),
            "flow_export" => self.flow_export(arguments),
            "propose_flow_update" => self.propose_flow_update(arguments),
            "apply_flow_update" => self.apply_flow_update(arguments),
            "prepare_flow_review" => self.prepare_flow_review(arguments),
            "approve_flow_review" => self.approve_flow_review(arguments),
            _ => Ok(ToolCallResult::error(
                json!({ "ok": false, "error": "unknown tool" }),
            )),
        }
    }

    fn start_run(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        if let Some(blocked) = self.run_asset_blocked_result(run_id, "start_run") {
            return Ok(blocked);
        }
        let nodes = node_specs(arguments)?;
        validate_start_run_preconditions(self.state.runtime(), run_id, &nodes)?;
        if let Err(err) = self.ensure_run_asset_manifest(run_id) {
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "asset_preservation": {
                    "status": "failed",
                    "stage": "run_manifest",
                    "error": err.message
                },
                "error": "run asset preservation failed"
            })));
        }
        let activation_ids = nodes
            .iter()
            .map(|node| node.id().to_string())
            .collect::<Vec<_>>();
        let tmux = match self.start_run_tmux_metadata(run_id, arguments, &activation_ids) {
            Ok(tmux) => tmux,
            Err(err) if self.run_has_asset_preservation_failure(run_id) => {
                return Ok(ToolCallResult::error(json!({
                    "ok": false,
                    "run_id": run_id,
                    "asset_preservation": {
                        "status": "failed",
                        "stage": "preservation_error",
                        "error": "run asset preservation failed"
                    },
                    "error": err.message
                })));
            }
            Err(err) => return Err(err),
        };
        let activation_ids = match self.state.runtime_mut().start_run(run_id, nodes) {
            Ok(activation_ids) => activation_ids,
            Err(err) => {
                let original = ToolError::from_runtime(err);
                return Err(self.finalize_tmux_after_error(
                    run_id,
                    "runtime_start_failed",
                    original,
                ));
            }
        };
        self.state
            .remember_tmux_allocation(run_id, &tmux.window, &tmux.panes);
        let run_assets = self.run_assets_json(run_id);

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "activation_ids": activation_ids,
            "tmux": tmux.structured,
            "run_assets": run_assets
        })))
    }

    fn get_context(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        match optional_string(arguments, &["run_id", "runId"])? {
            Some(run_id) => {
                if !self.state.runtime().has_run(run_id) {
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
                let context = self.context_with_run_assets(run_id, context);
                Ok(ToolCallResult::ok(
                    json!({ "ok": true, "run_id": run_id, "context": context }),
                ))
            }
            None => {
                let snapshot = self.state.runtime_snapshot();
                let runs = snapshot
                    .runs
                    .iter()
                    .map(|run| self.context_with_run_assets(&run.run_id, run.to_context_json()))
                    .collect::<Vec<_>>();
                Ok(ToolCallResult::ok(json!({ "ok": true, "runs": runs })))
            }
        }
    }

    fn context_with_run_assets(&self, run_id: &str, mut context: Value) -> Value {
        if let Some(manifest) = self.state.run_assets.get(run_id)
            && let Some(object) = context.as_object_mut()
        {
            object.insert(
                "run_assets".to_string(),
                serde_json::to_value(manifest).unwrap_or(Value::Null),
            );
        }
        context
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
            .runtime_mut()
            .deliver_artifact(run_id, activation_id, artifact_key, payload)
            .map_err(ToolError::from_runtime)?;
        let record = self
            .state
            .runtime()
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
        if let Some(blocked) = self.run_asset_blocked_result(run_id, "fanout_from_artifact") {
            return Ok(blocked);
        }
        let node_id = require_string(arguments, &["node_id", "nodeId"])?;
        let artifact_key = require_string(arguments, &["artifact_key", "artifactKey", "key"])?;
        let node = node_spec_from_arguments(node_id, arguments)?;
        let activation_ids = self
            .state
            .runtime_mut()
            .fanout_from_artifact(run_id, &node, artifact_key)
            .map_err(ToolError::from_runtime)?;
        let tmux_allocations = match self.allocate_missing_tmux_panes(run_id) {
            Ok(allocations) => allocations,
            Err(err) if self.run_has_asset_preservation_failure(run_id) => {
                let _ = self
                    .state
                    .runtime_mut()
                    .set_run_status(run_id, runtime::RunStatus::Failed);
                return Ok(ToolCallResult::error(json!({
                    "ok": false,
                    "run_id": run_id,
                    "control": "fanout_from_artifact",
                    "node_id": node_id,
                    "artifact_key": artifact_key,
                    "asset_preservation": {
                        "status": "failed",
                        "stage": "preservation_error",
                        "error": "run asset preservation failed"
                    },
                    "error": err.message
                })));
            }
            Err(err) => return Err(err),
        };
        if self.run_has_asset_preservation_failure(run_id) {
            let _ = self
                .state
                .runtime_mut()
                .set_run_status(run_id, runtime::RunStatus::Failed);
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "control": "fanout_from_artifact",
                "node_id": node_id,
                "artifact_key": artifact_key,
                "tmux_allocations": tmux_allocations,
                "asset_preservation": {
                    "status": "failed",
                    "stage": "preservation_error",
                    "error": "run asset preservation failed"
                },
                "error": "run asset preservation failed"
            })));
        }
        let state = self.state.runtime().state();
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
            "activations": activations,
            "tmux_allocations": tmux_allocations
        })))
    }

    fn record_effect(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let activation_id = require_string(arguments, &["activation_id", "activationId"])?;
        let effect_key = require_string(arguments, &["effect_key", "effectKey", "key"])?;
        let payload = payload_string(arguments.get("payload"))?;
        self.state
            .runtime_mut()
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
        let mut board_version = self.state.runtime().state().board_version;
        for (index, (key, value)) in patch.iter().enumerate() {
            let mut board_patch = BoardPatch::new(key, value_as_string(value)?);
            if index == 0 {
                if let Some(expected_version) = expected_version {
                    board_patch = board_patch.expect_version(expected_version);
                }
            }
            board_version = self
                .state
                .runtime_mut()
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
        if let Some(blocked) = self.run_asset_blocked_result(run_id, "activate_node") {
            return Ok(blocked);
        }
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
            .runtime_mut()
            .activate_node(run_id, &node, None)
            .map_err(ToolError::from_runtime)?;
        let tmux_allocations = match self.allocate_missing_tmux_panes(run_id) {
            Ok(allocations) => allocations,
            Err(err) if self.run_has_asset_preservation_failure(run_id) => {
                let _ = self
                    .state
                    .runtime_mut()
                    .set_run_status(run_id, runtime::RunStatus::Failed);
                return Ok(ToolCallResult::error(json!({
                    "ok": false,
                    "run_id": run_id,
                    "control": "activate_node",
                    "node_id": node_id,
                    "asset_preservation": {
                        "status": "failed",
                        "stage": "preservation_error",
                        "error": "run asset preservation failed"
                    },
                    "error": err.message
                })));
            }
            Err(err) => return Err(err),
        };
        if self.run_has_asset_preservation_failure(run_id) {
            let _ = self
                .state
                .runtime_mut()
                .set_run_status(run_id, runtime::RunStatus::Failed);
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "control": "activate_node",
                "node_id": node_id,
                "activation_id": activation_id,
                "tmux_allocations": tmux_allocations,
                "asset_preservation": {
                    "status": "failed",
                    "stage": "preservation_error",
                    "error": "run asset preservation failed"
                },
                "error": "run asset preservation failed"
            })));
        }

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "node_id": node_id,
            "activation_id": activation_id,
            "tmux_allocations": tmux_allocations
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

        match self.state.runtime().validate_stop(run_id, activation_id) {
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
        if !self.state.runtime().has_run(run_id) {
            let mut structured = run_not_found_guidance(run_id);
            if let Some(object) = structured.as_object_mut() {
                object.insert("mode".to_string(), json!(flow_lock_mode_name(mode)));
                object.insert("lock_id".to_string(), json!(lock_id));
                object.insert("flow_lock_id".to_string(), json!(lock_id));
                object.insert("content_hash".to_string(), json!(provided_content_hash));
            }
            return Ok(ToolCallResult::error(structured));
        }

        let review_status = self.run_review_status_name(lock_id, false);
        let prepared_revision = match self.prepare_run_flow_revision(
            run_id,
            lock_id,
            provided_content_hash,
            &review_status,
        ) {
            Ok(revision_id) => revision_id,
            Err(err) => {
                let message = err.message;
                self.record_asset_preservation_error(run_id, None, None, "flow_package", &message);
                let _ = self
                    .state
                    .runtime_mut()
                    .set_run_status(run_id, runtime::RunStatus::Failed);
                return Ok(ToolCallResult::error(json!({
                    "ok": false,
                    "run_id": run_id,
                    "lock_id": lock_id,
                    "flow_lock_id": lock_id,
                    "content_hash": provided_content_hash,
                    "asset_preservation": {
                        "status": "failed",
                        "stage": "flow_package",
                        "error": message
                    },
                    "error": "run asset preservation failed"
                })));
            }
        };
        if let Err(err) = self
            .state
            .runtime_mut()
            .apply_flow_lock(run_id, mode, lock_id, provided_content_hash)
            .map_err(ToolError::from_runtime)
        {
            self.mark_prepared_flow_revision_failed(run_id, &prepared_revision, &err.message);
            return Err(err);
        }
        if let Err(err) = self.commit_run_flow_revision(run_id, &prepared_revision) {
            let message = err.message;
            self.record_asset_preservation_error(run_id, None, None, "flow_package", &message);
            let _ = self
                .state
                .runtime_mut()
                .set_run_status(run_id, runtime::RunStatus::Failed);
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "lock_id": lock_id,
                "flow_lock_id": lock_id,
                "content_hash": provided_content_hash,
                "asset_preservation": {
                    "status": "failed",
                    "stage": "flow_package",
                    "error": message
                },
                "error": "run asset preservation failed"
            })));
        }
        let tmux_captures = self.capture_existing_tmux_panes(run_id)?;
        self.remember_run_archive(run_id, lock_id, provided_content_hash, review_status);
        let run_assets = self.run_assets_json(run_id);

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "run_id": run_id,
            "mode": flow_lock_mode_name(mode),
            "lock_id": lock_id,
            "content_hash": provided_content_hash,
            "tmux_captures": tmux_captures,
            "run_assets": run_assets
        })))
    }

    fn preview_flow_routes(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        route_preview::preview_flow_routes(&self.state, arguments)
    }

    fn ensure_run_asset_manifest(&mut self, run_id: &str) -> Result<(), ToolError> {
        if self.state.run_assets.contains_key(run_id) {
            return Ok(());
        }
        let manifest = self
            .run_asset_store
            .start_run_manifest(run_id)
            .map_err(ToolError::from_run_asset)?;
        self.state.run_assets.insert(run_id.to_string(), manifest);
        Ok(())
    }

    fn prepare_run_flow_revision(
        &mut self,
        run_id: &str,
        lock_id: &str,
        content_hash: &str,
        review_status: &str,
    ) -> Result<String, ToolError> {
        let lock = self
            .state
            .flow_locks
            .get(lock_id)
            .cloned()
            .ok_or_else(|| ToolError::invalid("flow lock not found"))?;
        self.ensure_run_asset_manifest(run_id)?;
        let manifest = self
            .state
            .run_assets
            .get_mut(run_id)
            .expect("run asset manifest should exist after ensure");
        let revision = self
            .run_asset_store
            .prepare_flow_revision(manifest, &lock, content_hash, review_status)
            .map_err(ToolError::from_run_asset)?;
        Ok(revision.revision_id)
    }

    fn commit_run_flow_revision(
        &mut self,
        run_id: &str,
        revision_id: &str,
    ) -> Result<(), ToolError> {
        let manifest = self
            .state
            .run_assets
            .get_mut(run_id)
            .ok_or_else(|| ToolError::invalid("run asset manifest not found"))?;
        self.run_asset_store
            .commit_flow_revision_applied(manifest, revision_id)
            .map_err(ToolError::from_run_asset)?;
        Ok(())
    }

    fn mark_prepared_flow_revision_failed(&mut self, run_id: &str, revision_id: &str, error: &str) {
        if let Some(manifest) = self.state.run_assets.get_mut(run_id) {
            let _ = self
                .run_asset_store
                .mark_flow_revision_failed(manifest, revision_id, error);
        }
    }

    fn run_assets_json(&self, run_id: &str) -> Option<Value> {
        self.state
            .run_assets
            .get(run_id)
            .and_then(|manifest| serde_json::to_value(manifest).ok())
    }

    fn run_asset_blocked_result(&self, run_id: &str, control: &str) -> Option<ToolCallResult> {
        if !self.run_has_asset_preservation_failure(run_id) {
            return None;
        }
        Some(ToolCallResult::error(json!({
            "ok": false,
            "run_id": run_id,
            "control": control,
            "asset_preservation": {
                "status": "failed",
                "stage": "preservation_error",
                "error": "run asset preservation failed"
            },
            "error": "run asset preservation failed"
        })))
    }
}

#[derive(Debug, Default)]
struct McpServerState {
    driver: DriverState,
    flow_locks: BTreeMap<String, flow::FlowLock>,
    reviews: BTreeMap<String, FlowReviewRecord>,
    flow_review_index: BTreeMap<String, String>,
    proposed_updates: BTreeMap<String, ProposedFlowUpdate>,
    messages: BTreeMap<String, Vec<Value>>,
    tmux_windows: BTreeMap<String, TmuxWindow>,
    tmux_panes: BTreeMap<(String, String), TmuxPane>,
    tmux_pipe_captures: BTreeMap<(String, String), TmuxPipeCapture>,
    tmux_final_captures: BTreeMap<(String, String), String>,
    released_tmux_panes: BTreeSet<(String, String)>,
    run_archives: BTreeMap<String, RunArchive>,
    run_agent_commands: BTreeMap<String, String>,
    machine_inputs: BTreeMap<String, Vec<Value>>,
    actuation_warnings: BTreeMap<String, Vec<Value>>,
    run_assets: BTreeMap<String, RunAssetManifest>,
    shutdown_requested: bool,
    shutdown_assets_finalized: bool,
    shutdown_assets_error: Option<String>,
    shutdown_assets_summary: Option<Value>,
    launched_activations: BTreeSet<(String, String)>,
    actuated_activations: BTreeSet<(String, String)>,
}

impl McpServerState {
    fn runtime(&self) -> &Runtime {
        self.driver.runtime()
    }

    fn runtime_mut(&mut self) -> &mut Runtime {
        self.driver.runtime_mut()
    }

    fn tick_control(&mut self, control: ControlCommand) -> runtime::DriverTickReport {
        self.driver
            .tick(self.driver_tick_input().with_control(control))
    }

    fn tick_stop_observation(
        &mut self,
        run_id: &str,
        activation_id: &str,
        observation: runtime::StopObservation,
    ) -> runtime::DriverTickReport {
        self.driver
            .tick(self.driver_tick_input().with_stop_observation(
                run_id,
                activation_id,
                observation,
            ))
    }

    fn driver_tick_input(&self) -> DriverTickInput {
        let mut input = DriverTickInput::default();
        let lock_ids = self
            .runtime()
            .state()
            .flow_lock_id_by_run
            .values()
            .cloned()
            .collect::<BTreeSet<_>>();
        for lock_id in lock_ids {
            if let Some(lock) = self.flow_locks.get(&lock_id) {
                input = input.with_route_lock(lock.clone());
            }
        }
        input
    }

    fn remember_tmux_allocation(
        &mut self,
        run_id: &str,
        window: &Option<TmuxWindow>,
        panes: &[TmuxPane],
    ) {
        if let Some(window) = window {
            self.tmux_windows.insert(run_id.to_string(), window.clone());
        }
        for pane in panes {
            self.tmux_panes.insert(
                (run_id.to_string(), pane.activation_id().to_string()),
                pane.clone(),
            );
        }
    }

    fn runtime_snapshot(&self) -> VisualizationSnapshot {
        let mut snapshot =
            VisualizationSnapshot::from_runtime(self.runtime().state(), &self.message_counts());
        self.enrich_runtime_snapshot(&mut snapshot);
        snapshot
    }

    fn message_counts(&self) -> BTreeMap<String, usize> {
        self.messages
            .iter()
            .map(|(run_id, messages)| (run_id.clone(), messages.len()))
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
struct RunArchive {
    flow_lock_id: String,
    content_hash: String,
    review_status: String,
    flow_export_document: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct FlowReviewRecord {
    review_id: String,
    lock_id: String,
    content_hash: String,
    status: FlowReviewStatus,
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum FlowReviewStatus {
    Pending,
    Approved,
    Bypassed,
    Rejected,
}

impl FlowReviewStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Bypassed => "bypassed",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ProposedFlowUpdate {
    mode: runtime::FlowLockMode,
    content_hash: String,
    summary: String,
    review_required: bool,
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

const SERVER_INSTRUCTIONS: &str = "When a user asks to use Humanize or workflow, start with flow_suggest from the terse natural-language request, then call flow_check, flow_lock, prepare_flow_review, approve_flow_review with an approved or bypassed decision, and run_flow; do not substitute ordinary repo exploration for this workflow. Validate that the flow package includes a README before locking or running.";

fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {}
        },
        "instructions": SERVER_INSTRUCTIONS,
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

    fn from_run_asset(err: crate::run_assets::RunAssetError) -> Self {
        Self::invalid(format!("run asset preservation {err}"))
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

fn optional_bool(arguments: &Value, names: &[&str]) -> Result<Option<bool>, ToolError> {
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

fn optional_bool_field(
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

fn optional_flow_lock_mode_arg(
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

fn flow_repair_input_arg(arguments: &Value) -> Result<flow::FlowRepairInput, ToolError> {
    let mode = flow_check_mode_arg(arguments)?;
    let flow = require_object_arg(arguments, &["flow"])?;
    let draft = flow_draft_for_repair(flow)?;
    let route_authoring = route_authoring_arg(arguments, flow)?;

    Ok(flow::FlowRepairInput {
        draft,
        mode,
        route_authoring,
        diagnostics: Vec::new(),
    })
}

fn route_authoring_arg(
    arguments: &Value,
    flow: &serde_json::Map<String, Value>,
) -> Result<Vec<flow::RouteAuthoring>, ToolError> {
    let source = match arguments.get("route_authoring") {
        Some(value) => value
            .as_array()
            .ok_or_else(|| ToolError::invalid("route_authoring must be an array"))?,
        None => optional_array_field(flow, "routes")?,
    };

    source.iter().map(parse_route_authoring).collect()
}

fn parse_route_authoring(value: &Value) -> Result<flow::RouteAuthoring, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("route_authoring items must be objects"))?;

    Ok(flow::RouteAuthoring {
        when: optional_string_field(object, &["when"])?.map(str::to_string),
        predicate: object
            .get("predicate")
            .map(parse_route_predicate)
            .transpose()?,
        to: optional_string_field(object, &["to"])?.map(str::to_string),
        activate: optional_string_field(object, &["activate"])?.map(str::to_string),
        for_each: optional_string_field(object, &["for_each", "forEach"])?.map(str::to_string),
    })
}

fn parse_route_predicate(value: &Value) -> Result<flow::RoutePredicateDraft, ToolError> {
    match value {
        Value::String(value) => Ok(flow::RoutePredicateDraft::Text(value.clone())),
        Value::Object(object) => Ok(flow::RoutePredicateDraft::Artifact(
            string_field(object, &["artifact"])?.to_string(),
        )),
        _ => Err(ToolError::invalid(
            "predicate must be a string or an object with artifact",
        )),
    }
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

fn input_severity_name(diagnostics: &[flow::Diagnostic]) -> &'static str {
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity_level == flow::DiagnosticSeverity::Fatal)
    {
        "fatal"
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity_level == flow::DiagnosticSeverity::Error)
    {
        "error"
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity_level == flow::DiagnosticSeverity::Warning)
    {
        "warning"
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity_level == flow::DiagnosticSeverity::Note)
    {
        "note"
    } else {
        "none"
    }
}

fn repair_patches_json(patches: &[flow::FlowRepairPatch]) -> Vec<Value> {
    patches
        .iter()
        .map(|patch| {
            json!({
                "repair_kind": repair_kind_name(patch.repair_kind),
                "location": patch.location,
                "replacement": patch.replacement
            })
        })
        .collect()
}

fn repair_candidates_json(candidates: &[flow::FlowRepairCandidate]) -> Vec<Value> {
    candidates
        .iter()
        .map(|candidate| {
            json!({
                "repair_kind": repair_kind_name(candidate.repair_kind),
                "location": candidate.location,
                "replacement": candidate.replacement
            })
        })
        .collect()
}

fn repair_guidance_json(diagnostics: &[flow::Diagnostic]) -> Vec<Value> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code,
                "location": diagnostic.location,
                "message": diagnostic.message,
                "fix_hint": diagnostic.fix_hint,
                "why_it_matters": diagnostic.why_it_matters,
                "repairability": repairability_name(diagnostic.repairability),
                "repair_kinds": diagnostic
                    .repair_kinds
                    .iter()
                    .map(|kind| repair_kind_name(*kind))
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

fn diagnostics_json(diagnostics: &[flow::Diagnostic]) -> Vec<Value> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code,
                "severity": severity_name(diagnostic.severity),
                "severity_level": diagnostic_severity_name(diagnostic.severity_level),
                "domain": diagnostic_domain_name(diagnostic.domain),
                "repairability": repairability_name(diagnostic.repairability),
                "location": diagnostic.location,
                "message": diagnostic.message,
                "fix_hint": diagnostic.fix_hint,
                "why_it_matters": diagnostic.why_it_matters,
                "repair_kinds": diagnostic
                    .repair_kinds
                    .iter()
                    .map(|kind| repair_kind_name(*kind))
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

fn diagnostic_codes_text(diagnostics: &[flow::Diagnostic]) -> String {
    if diagnostics.is_empty() {
        "none".to_string()
    } else {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn severity_name(severity: flow::Severity) -> &'static str {
    match severity {
        flow::Severity::Error => "error",
        flow::Severity::Warning => "warning",
    }
}

fn diagnostic_severity_name(severity: flow::DiagnosticSeverity) -> &'static str {
    match severity {
        flow::DiagnosticSeverity::Fatal => "fatal",
        flow::DiagnosticSeverity::Error => "error",
        flow::DiagnosticSeverity::Warning => "warning",
        flow::DiagnosticSeverity::Note => "note",
    }
}

fn diagnostic_domain_name(domain: flow::DiagnosticDomain) -> &'static str {
    match domain {
        flow::DiagnosticDomain::Package => "package",
        flow::DiagnosticDomain::Contract => "contract",
        flow::DiagnosticDomain::Resource => "resource",
        flow::DiagnosticDomain::Route => "route",
        flow::DiagnosticDomain::Policy => "policy",
        flow::DiagnosticDomain::RuntimeCompat => "runtime_compat",
    }
}

fn repairability_name(repairability: flow::Repairability) -> &'static str {
    match repairability {
        flow::Repairability::Automatic => "automatic",
        flow::Repairability::Candidate => "candidate",
        flow::Repairability::GuidanceOnly => "guidance_only",
        flow::Repairability::None => "none",
    }
}

fn repair_kind_name(kind: flow::RepairKind) -> &'static str {
    match kind {
        flow::RepairKind::RouteWhenToPredicate => "route_when_to_predicate",
        flow::RepairKind::RouteToToActivate => "route_to_to_activate",
        flow::RepairKind::RouteArtifactObjectToExists => "route_artifact_object_to_exists",
        flow::RepairKind::RouteBareArtifactDeliveredToExists => {
            "route_bare_artifact_delivered_to_exists"
        }
        flow::RepairKind::AddRouteTarget => "add_route_target",
        flow::RepairKind::AddReadmeResource => "add_readme_resource",
        flow::RepairKind::GenerateReadme => "generate_readme",
        flow::RepairKind::AddArtifactSchema => "add_artifact_schema",
        flow::RepairKind::AddContractCompletion => "add_contract_completion",
        flow::RepairKind::NarrowWriteScope => "narrow_write_scope",
        flow::RepairKind::ProvideRuntimeResource => "provide_runtime_resource",
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
