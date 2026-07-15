use std::collections::BTreeMap;

use crate::adapters::tmux::{CommandRunner, SystemCommandRunner, TmuxAdapter};
use crate::review::ReviewStore;
use crate::run_assets::{RunAssetSink, RunAssetStore};
use crate::{flow, runtime};
use serde_json::{Value, json};

mod arguments;
mod driver_attach;
mod driver_cleanup;
mod driver_proxy;
mod driver_run_flow;
mod execution_defaults;
mod flow_binding;
mod flow_json;
mod flow_parse;
mod flow_tools;
mod flow_updates;
mod participant;
mod registry;
#[cfg(test)]
mod registry_contract_tests;
mod review_tools;
mod stdio;
mod surface;
mod tool_schemas;

pub use execution_defaults::TmuxExecutionDefaults;
pub use stdio::{
    serve_stdio, serve_stdio_signal_aware, serve_stdio_signal_aware_with_server,
    serve_stdio_with_server,
};
pub use surface::{McpSurface, McpToolDescriptor};

use arguments::*;
use flow_json::{
    diagnostics_json, input_severity_name, repair_candidates_json, repair_guidance_json,
};
use flow_parse::{flow_draft_arg, flow_draft_is_empty};
use participant::McpCaller;
use registry::{AuthoringOperation, ToolRoute, ToolSpec};

pub struct McpServer<R: CommandRunner = SystemCommandRunner> {
    surface: McpSurface,
    state: McpServerState,
    tmux_adapter: TmuxAdapter<R>,
    run_asset_store: RunAssetStore,
    review_store: ReviewStore,
    execution_defaults: TmuxExecutionDefaults,
    caller: McpCaller,
}

impl McpServer<SystemCommandRunner> {
    pub fn new() -> Self {
        Self::with_tmux_runner_run_asset_store_and_execution_defaults(
            SystemCommandRunner,
            RunAssetStore::runtime_default(),
            TmuxExecutionDefaults::from_environment(),
        )
    }

    pub(crate) fn from_environment() -> std::io::Result<Self> {
        Self::with_system_dependencies_and_caller(McpCaller::from_environment()?)
    }

    fn with_system_dependencies_and_caller(caller: McpCaller) -> std::io::Result<Self> {
        let run_asset_store = caller
            .runs_root()
            .map_or_else(RunAssetStore::runtime_default, |root| {
                RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.to_path_buf()))
            });
        let review_store = ReviewStore::runtime_default().map_err(|err| {
            std::io::Error::new(err.kind(), format!("MCP configuration error: {err}"))
        })?;
        Ok(Self {
            surface: McpSurface,
            state: McpServerState::default(),
            tmux_adapter: TmuxAdapter::with_runner(SystemCommandRunner),
            run_asset_store,
            review_store,
            execution_defaults: TmuxExecutionDefaults::from_environment(),
            caller,
        })
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
        Self::with_tmux_runner_run_asset_store_and_execution_defaults(
            runner,
            run_asset_store,
            TmuxExecutionDefaults::default(),
        )
    }

    pub fn with_tmux_runner_run_asset_store_and_execution_defaults(
        runner: R,
        run_asset_store: RunAssetStore,
        execution_defaults: TmuxExecutionDefaults,
    ) -> Self {
        let review_store = ReviewStore::new(
            run_asset_store
                .runs_root()
                .unwrap_or_else(|_| std::env::temp_dir().join("humanize-runs"))
                .join("reviews"),
        );
        Self {
            surface: McpSurface,
            state: McpServerState::default(),
            tmux_adapter: TmuxAdapter::with_runner(runner),
            run_asset_store,
            review_store,
            execution_defaults,
            caller: McpCaller::Operator,
        }
    }

    pub fn with_review_store(mut self, review_store: ReviewStore) -> Self {
        self.review_store = review_store;
        self
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
            "tools/list" => Some(success_response(
                id,
                self.surface.tools_list_json_for(self.caller.kind()),
            )),
            "tools/call" => Some(self.handle_tool_call(id, request.get("params"))),
            _ => Some(error_response(id, -32601, "method not found")),
        }
    }

    fn handle_shutdown(&mut self, id: Option<Value>) -> Value {
        self.state.shutdown_requested = true;
        success_response(id, json!({ "ok": true, "shutdown": true }))
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
        let Some(spec) = registry::advertised_spec_for(name, self.caller.kind()) else {
            return error_response(id, -32602, "unknown tool");
        };
        let Some(arguments) = params.get("arguments") else {
            return error_response(id, -32602, "tools/call params.arguments must be an object");
        };
        if !arguments.is_object() {
            return error_response(id, -32602, "tools/call params.arguments must be an object");
        }

        match self.call_tool(spec, arguments) {
            Ok(tool_result) => success_response(id, tool_result.to_json()),
            Err(err) => error_response(id, -32602, &err.message),
        }
    }

    fn call_tool(
        &mut self,
        spec: &'static ToolSpec,
        arguments: &Value,
    ) -> Result<ToolCallResult, ToolError> {
        match spec.route {
            ToolRoute::Authoring(operation) => {
                let context = driver_proxy::ToolArgumentContext::new(&self.state, &self.caller);
                let arguments = (spec.prepare_arguments)(&context, arguments)?;
                self.call_authoring_operation(operation, &arguments)
            }
            ToolRoute::DriverMutation {
                bootstrap: true, ..
            } => {
                let context = driver_proxy::ToolArgumentContext::new(&self.state, &self.caller);
                let arguments = (spec.prepare_arguments)(&context, arguments)?;
                self.run_flow_with_driver(&arguments)
            }
            ToolRoute::DriverRead(_)
            | ToolRoute::DriverMutation {
                bootstrap: false, ..
            }
            | ToolRoute::ParticipantMessage(_) => {
                let wire = spec
                    .route
                    .wire()
                    .ok_or_else(|| ToolError::invalid("tool has no driver wire operation"))?;
                self.proxy_driver_owned_run_tool(spec.name, wire, spec.prepare_arguments, arguments)
            }
            ToolRoute::Hidden => Err(ToolError::invalid("unknown tool")),
        }
    }

    fn call_authoring_operation(
        &mut self,
        operation: AuthoringOperation,
        arguments: &Value,
    ) -> Result<ToolCallResult, ToolError> {
        match operation {
            AuthoringOperation::FlowRepair => self.flow_repair(arguments),
            AuthoringOperation::FlowApply => self.flow_apply(arguments),
            AuthoringOperation::FlowSuggest => self.flow_suggest(arguments),
            AuthoringOperation::FlowCheck => self.flow_check(arguments),
            AuthoringOperation::FlowLock => self.flow_lock(arguments),
            AuthoringOperation::FlowExport => self.flow_export(arguments),
            AuthoringOperation::ProposeFlowUpdate => self.propose_flow_update(arguments),
            AuthoringOperation::PrepareFlowReview => self.prepare_flow_review(arguments),
            AuthoringOperation::DecideFlowReview => self.decide_flow_review(arguments),
        }
    }
}

#[derive(Debug, Default)]
struct McpServerState {
    flow_locks: BTreeMap<String, flow::FlowLock>,
    proposed_updates: BTreeMap<String, ProposedFlowUpdate>,
    shutdown_requested: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ProposedFlowUpdate {
    mode: runtime::FlowLockMode,
    content_hash: String,
    summary: String,
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
            "content": [{ "type": "text", "text": text }],
            "structuredContent": self.structured,
            "isError": self.is_error
        })
    }
}

const SERVER_INSTRUCTIONS: &str = "When a user asks to use Humanize or workflow, start with flow_suggest from the terse natural-language request, then call flow_check, flow_lock, prepare_flow_review, decide_flow_review with an approved or bypassed decision, and run_flow; do not substitute ordinary repo exploration for this workflow. Validate that the flow package includes a root README.md before locking or running. Agent and review nodes require an autonomous tmux context from HUMANIZE_TMUX_SESSION and HUMANIZE_AGENT_COMMAND, or run_flow fails before starting.";

fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
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
        "error": { "code": code, "message": message }
    })
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ToolError {
    message: String,
    diagnostic: Option<String>,
}

impl ToolError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            diagnostic: None,
        }
    }

    fn private_failure(message: impl Into<String>, diagnostic: impl ToString) -> Self {
        Self {
            message: message.into(),
            diagnostic: Some(diagnostic.to_string()),
        }
    }

    fn diagnostic(&self) -> &str {
        self.diagnostic.as_deref().unwrap_or(&self.message)
    }

    fn missing(name: &str) -> Self {
        Self::invalid(format!("missing required argument: {name}"))
    }

    fn from_tmux(err: crate::adapters::tmux::TmuxError) -> Self {
        Self::invalid(format!("tmux {err}"))
    }

    fn from_run_asset(err: crate::run_assets::RunAssetError) -> Self {
        Self::invalid(format!("run asset preservation {err}"))
    }

    fn from_io(err: std::io::Error) -> Self {
        Self::private_failure("runtime I/O operation failed", err)
    }
}
