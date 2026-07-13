use serde::Serialize;
use serde_json::{Value, json};

pub const RUNTIME_TOOL_NAMES: [&str; 21] = [
    "start_run",
    "get_context",
    "deliver_artifact",
    "fanout_from_artifact",
    "record_effect",
    "patch_board",
    "activate_node",
    "send_message",
    "validate_stop",
    "observe_stop",
    "apply_flow_lock",
    "preview_flow_routes",
    "run_flow",
    "run_status",
    "run_why",
    "pause_run",
    "resume_run",
    "stop_run",
    "view_terminal",
    "view_snapshot",
    "view_browser",
];

pub const AUTHORING_TOOL_NAMES: [&str; 8] = [
    "flow_repair",
    "flow_apply",
    "flow_suggest",
    "flow_check",
    "flow_lock",
    "flow_export",
    "propose_flow_update",
    "apply_flow_update",
];

pub const REVIEW_TOOL_NAMES: [&str; 2] = ["prepare_flow_review", "approve_flow_review"];

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct McpSurface;

impl McpSurface {
    pub fn runtime_tools(&self) -> Vec<McpToolDescriptor> {
        RUNTIME_TOOL_NAMES
            .iter()
            .map(|name| descriptor_for(name))
            .collect()
    }

    pub fn authoring_tools(&self) -> Vec<McpToolDescriptor> {
        AUTHORING_TOOL_NAMES
            .iter()
            .map(|name| descriptor_for(name))
            .collect()
    }

    pub fn review_tools(&self) -> Vec<McpToolDescriptor> {
        REVIEW_TOOL_NAMES
            .iter()
            .map(|name| descriptor_for(name))
            .collect()
    }

    pub fn tools(&self) -> Vec<McpToolDescriptor> {
        RUNTIME_TOOL_NAMES
            .iter()
            .chain(AUTHORING_TOOL_NAMES.iter())
            .chain(REVIEW_TOOL_NAMES.iter())
            .map(|name| descriptor_for(name))
            .collect()
    }

    pub fn lookup(&self, name: &str) -> Option<McpToolDescriptor> {
        RUNTIME_TOOL_NAMES
            .iter()
            .chain(AUTHORING_TOOL_NAMES.iter())
            .chain(REVIEW_TOOL_NAMES.iter())
            .find(|tool_name| *tool_name == &name)
            .map(|tool_name| descriptor_for(tool_name))
    }

    pub fn tools_list_json(&self) -> Value {
        json!({ "tools": self.tools() })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpToolDescriptor {
    name: &'static str,
    description: &'static str,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

impl McpToolDescriptor {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn description(&self) -> &'static str {
        self.description
    }

    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }
}

fn descriptor_for(name: &str) -> McpToolDescriptor {
    match name {
        "start_run" => descriptor(
            "start_run",
            "Call start_run to create a runtime run before using runtime tools; omit nodes to create root.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "nodes": {
                        "type": "array",
                        "items": {
                            "oneOf": [
                                { "type": "string" },
                                {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string" },
                                        "required_artifacts": {
                                            "type": "array",
                                            "items": { "type": "string" }
                                        },
                                        "required_effects": {
                                            "type": "array",
                                            "items": { "type": "string" }
                                        }
                                    },
                                    "required": ["id"]
                                }
                            ]
                        }
                    },
                    "required_artifacts": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "required_effects": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "tmux": {
                        "type": "object",
                        "description": "tmux mapping options. When enabled is true, session and window are required.",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "session": { "type": "string" },
                            "window": { "type": "string" }
                        },
                        "allOf": [
                            {
                                "if": {
                                    "properties": {
                                        "enabled": { "const": true }
                                    },
                                    "required": ["enabled"]
                                },
                                "then": {
                                    "required": ["session", "window"]
                                }
                            }
                        ]
                    }
                }),
                &[],
                &[&["run_id", "runId"]],
            ),
        ),
        "get_context" => descriptor(
            "get_context",
            "Return local context for one run or all in-memory runs.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" }
                }),
                &[],
            ),
        ),
        "deliver_artifact" => descriptor(
            "deliver_artifact",
            "Record an artifact payload for a run activation.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "activation_id": { "type": "string" },
                    "activationId": { "type": "string" },
                    "artifact_key": { "type": "string" },
                    "artifactKey": { "type": "string" },
                    "key": { "type": "string" },
                    "payload": {}
                }),
                &[],
                &[
                    &["run_id", "runId"],
                    &["activation_id", "activationId"],
                    &["artifact_key", "artifactKey", "key"],
                ],
            ),
        ),
        "fanout_from_artifact" => descriptor(
            "fanout_from_artifact",
            "Create one runtime activation per line in the latest artifact slot.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "node_id": { "type": "string" },
                    "nodeId": { "type": "string" },
                    "artifact_key": { "type": "string" },
                    "artifactKey": { "type": "string" },
                    "key": { "type": "string" },
                    "for_each": { "type": "string" },
                    "forEach": { "type": "string" },
                    "required_artifacts": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "required_effects": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }),
                &[],
                &[
                    &["run_id", "runId"],
                    &["node_id", "nodeId"],
                    &["artifact_key", "artifactKey", "key"],
                ],
            ),
        ),
        "record_effect" => descriptor(
            "record_effect",
            "Record an effect fact for a run activation.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "activation_id": { "type": "string" },
                    "activationId": { "type": "string" },
                    "effect_key": { "type": "string" },
                    "effectKey": { "type": "string" },
                    "key": { "type": "string" },
                    "payload": {}
                }),
                &[],
                &[
                    &["run_id", "runId"],
                    &["activation_id", "activationId"],
                    &["effect_key", "effectKey", "key"],
                ],
            ),
        ),
        "patch_board" => descriptor(
            "patch_board",
            "Patch local run board values with optional version checking.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "activation_id": { "type": "string" },
                    "activationId": { "type": "string" },
                    "expected_version": { "type": "integer", "minimum": 0 },
                    "expectedVersion": { "type": "integer", "minimum": 0 },
                    "patch": { "type": "object" }
                }),
                &["patch"],
                &[&["run_id", "runId"], &["activation_id", "activationId"]],
            ),
        ),
        "activate_node" => descriptor(
            "activate_node",
            "Create runtime metadata for a node activation.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "node_id": { "type": "string" },
                    "nodeId": { "type": "string" },
                    "activation_id": { "type": "string" },
                    "activationId": { "type": "string" }
                }),
                &[],
                &[&["run_id", "runId"], &["node_id", "nodeId"]],
            ),
        ),
        "send_message" => descriptor(
            "send_message",
            "Store a local message associated with a run.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "message": {}
                }),
                &["message"],
                &[&["run_id", "runId"]],
            ),
        ),
        "validate_stop" => descriptor(
            "validate_stop",
            "Validate whether a runtime activation satisfies its local stop contract.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "activation_id": { "type": "string" },
                    "activationId": { "type": "string" }
                }),
                &[],
                &[&["run_id", "runId"], &["activation_id", "activationId"]],
            ),
        ),
        "observe_stop" => descriptor(
            "observe_stop",
            "Record an observed activation stop and let the runtime driver validate completion.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "activation_id": { "type": "string" },
                    "activationId": { "type": "string" },
                    "reason": { "type": "string" }
                }),
                &["reason"],
                &[&["run_id", "runId"], &["activation_id", "activationId"]],
            ),
        ),
        "apply_flow_lock" => descriptor(
            "apply_flow_lock",
            "Apply a flow lock to an existing runtime run; if the run is missing, call start_run first.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "mode": {
                        "type": "string",
                        "enum": ["future_activations", "checkpoint_restart"]
                    },
                    "lock_id": { "type": "string" },
                    "lockId": { "type": "string" },
                    "flow_lock_id": { "type": "string" },
                    "flowLockId": { "type": "string" },
                    "content_hash": { "type": "string" },
                    "contentHash": { "type": "string" }
                }),
                &["mode"],
                &[
                    &["run_id", "runId"],
                    &["lock_id", "lockId", "flow_lock_id", "flowLockId"],
                    &["content_hash", "contentHash"],
                ],
            ),
        ),
        "preview_flow_routes" => descriptor(
            "preview_flow_routes",
            "Preview typed flow lock route activations for an existing runtime run; if the run is missing, call start_run first.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "flow_lock_id": { "type": "string" },
                    "flowLockId": { "type": "string" },
                    "lock_id": { "type": "string" },
                    "lockId": { "type": "string" },
                    "content_hash": { "type": "string" },
                    "contentHash": { "type": "string" }
                }),
                &[],
                &[&["run_id", "runId"]],
            ),
        ),
        "run_flow" => descriptor(
            "run_flow",
            "Create a runtime run through the driver control surface, with optional flow review enforcement.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "nodes": {
                        "type": "array",
                        "items": {
                            "oneOf": [
                                { "type": "string" },
                                {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string" },
                                        "required_artifacts": {
                                            "type": "array",
                                            "items": { "type": "string" }
                                        },
                                        "required_effects": {
                                            "type": "array",
                                            "items": { "type": "string" }
                                        }
                                    },
                                    "required": ["id"]
                                }
                            ]
                        }
                    },
                    "flow": {},
                    "flow_lock_id": { "type": "string" },
                    "flowLockId": { "type": "string" },
                    "lock_id": { "type": "string" },
                    "lockId": { "type": "string" },
                    "content_hash": { "type": "string" },
                    "contentHash": { "type": "string" },
                    "review_required": { "type": "boolean" },
                    "reviewRequired": { "type": "boolean" },
                    "tmux": {
                        "type": "object",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "session": { "type": "string" },
                            "window": { "type": "string" },
                            "agent_command": {
                                "type": "string",
                                "description": "Command to launch the node agent inside each tmux pane before the node prompt is submitted."
                            },
                            "agentCommand": {
                                "type": "string",
                                "description": "Alias for agent_command."
                            },
                            "prompt_submit_key_count": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": 4
                            },
                            "promptSubmitKeyCount": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": 4
                            },
                            "agent_ready_pattern": {
                                "type": "string",
                                "minLength": 1
                            },
                            "agentReadyPattern": {
                                "type": "string",
                                "minLength": 1
                            },
                            "agent_ready_timeout_ms": {
                                "type": "integer",
                                "minimum": 100,
                                "maximum": 300000
                            },
                            "agentReadyTimeoutMs": {
                                "type": "integer",
                                "minimum": 100,
                                "maximum": 300000
                            }
                        }
                    }
                }),
                &[],
                &[&["run_id", "runId"]],
            ),
        ),
        "run_status" => descriptor(
            "run_status",
            "Return driver run status and runtime context for one run.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" }
                }),
                &[],
                &[&["run_id", "runId"]],
            ),
        ),
        "run_why" => descriptor(
            "run_why",
            "Return a concise reason for the current run state.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" }
                }),
                &[],
                &[&["run_id", "runId"]],
            ),
        ),
        "pause_run" => descriptor(
            "pause_run",
            "Pause an existing run through the runtime driver control path.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" }
                }),
                &[],
                &[&["run_id", "runId"]],
            ),
        ),
        "resume_run" => descriptor(
            "resume_run",
            "Resume an existing run through the runtime driver control path.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" }
                }),
                &[],
                &[&["run_id", "runId"]],
            ),
        ),
        "stop_run" => descriptor(
            "stop_run",
            "Request run stopping through the runtime driver control path.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" }
                }),
                &[],
                &[&["run_id", "runId"]],
            ),
        ),
        "view_terminal" => descriptor(
            "view_terminal",
            "Render the current in-memory runtime snapshot as terminal text.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" }
                }),
                &[],
            ),
        ),
        "view_snapshot" => descriptor(
            "view_snapshot",
            "Return the current in-memory runtime snapshot as structured JSON.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" }
                }),
                &[],
            ),
        ),
        "view_browser" => descriptor(
            "view_browser",
            "Serve the current in-memory runtime snapshot on a local read-only HTTP port.",
            object_schema(
                json!({
                    "host": { "type": "string" },
                    "port": { "type": "integer", "minimum": 0, "maximum": 65535 }
                }),
                &[],
            ),
        ),
        "flow_repair" => descriptor(
            "flow_repair",
            "Run flow authoring repair analysis and return mechanical patches, candidates, and guidance.",
            object_schema(
                json!({
                    "flow": {},
                    "mode": { "type": "string" },
                    "route_authoring": {
                        "type": "array",
                        "items": { "type": "object" }
                    }
                }),
                &["flow"],
            ),
        ),
        "flow_apply" => descriptor(
            "flow_apply",
            "Record that a supplied or locked flow was selected for application.",
            object_schema(
                json!({
                    "flow": {},
                    "flow_lock_id": { "type": "string" },
                    "flowLockId": { "type": "string" },
                    "lock_id": { "type": "string" },
                    "lockId": { "type": "string" }
                }),
                &[],
            ),
        ),
        "flow_suggest" => descriptor(
            "flow_suggest",
            "Humanize entry for terse natural-language workflow requests. Use first when the user asks to design or use a Humanize flow; then call flow_check, flow_lock, prepare_flow_review, and run_flow as needed.",
            object_schema(
                json!({
                    "goal": { "type": "string" },
                    "nodes": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "artifact": { "type": "string" }
                }),
                &["goal"],
            ),
        ),
        "flow_check" => descriptor(
            "flow_check",
            "Validate a Humanize flow draft before locking, review, export, or runtime execution.",
            object_schema(
                json!({ "flow": {}, "mode": { "type": "string" } }),
                &["flow"],
            ),
        ),
        "flow_lock" => descriptor(
            "flow_lock",
            "Freeze a validated Humanize flow draft into a deterministic lock for review, export, and runtime execution.",
            object_schema(
                json!({ "flow": {}, "mode": { "type": "string" } }),
                &["flow"],
            ),
        ),
        "flow_export" => descriptor(
            "flow_export",
            "Export a known flow lock through the flow authoring exporter.",
            object_schema_with_required_aliases(
                json!({
                    "flow_lock_id": { "type": "string" },
                    "flowLockId": { "type": "string" },
                    "lock_id": { "type": "string" },
                    "lockId": { "type": "string" },
                    "format": { "type": "string", "enum": ["json", "yaml"] }
                }),
                &[],
                &[&["flow_lock_id", "flowLockId", "lock_id", "lockId"]],
            ),
        ),
        "propose_flow_update" => descriptor(
            "propose_flow_update",
            "Check and lock a candidate flow update before runtime application.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "flow": {},
                    "apply_mode": {
                        "type": "string",
                        "enum": ["future_activations", "checkpoint_restart"]
                    },
                    "applyMode": {
                        "type": "string",
                        "enum": ["future_activations", "checkpoint_restart"]
                    },
                    "summary": { "type": "string" },
                    "review_required": { "type": "boolean" },
                    "reviewRequired": { "type": "boolean" }
                }),
                &["flow"],
            ),
        ),
        "apply_flow_update" => descriptor(
            "apply_flow_update",
            "Apply a previously proposed flow update to an existing runtime run.",
            object_schema_with_required_aliases(
                json!({
                    "run_id": { "type": "string" },
                    "runId": { "type": "string" },
                    "flow_lock_id": { "type": "string" },
                    "flowLockId": { "type": "string" },
                    "lock_id": { "type": "string" },
                    "lockId": { "type": "string" },
                    "content_hash": { "type": "string" },
                    "contentHash": { "type": "string" },
                    "apply_mode": {
                        "type": "string",
                        "enum": ["future_activations", "checkpoint_restart"]
                    },
                    "applyMode": {
                        "type": "string",
                        "enum": ["future_activations", "checkpoint_restart"]
                    }
                }),
                &[],
                &[
                    &["run_id", "runId"],
                    &["flow_lock_id", "flowLockId", "lock_id", "lockId"],
                    &["content_hash", "contentHash"],
                ],
            ),
        ),
        "prepare_flow_review" => descriptor(
            "prepare_flow_review",
            "Create a human-readable review document for a Humanize flow lock or draft before long-running execution.",
            object_schema(
                json!({
                    "flow": {},
                    "flow_lock_id": { "type": "string" },
                    "flowLockId": { "type": "string" },
                    "lock_id": { "type": "string" },
                    "lockId": { "type": "string" },
                    "content_hash": { "type": "string" },
                    "contentHash": { "type": "string" },
                    "title": { "type": "string" }
                }),
                &[],
            ),
        ),
        "approve_flow_review" => descriptor(
            "approve_flow_review",
            "Record the human review decision for a Humanize flow before reviewed runtime execution.",
            object_schema(
                json!({
                    "review_id": { "type": "string" },
                    "reviewId": { "type": "string" },
                    "decision": {
                        "type": "string",
                        "enum": ["approved", "bypassed", "rejected"]
                    },
                    "reason": { "type": "string" }
                }),
                &["review_id", "decision"],
            ),
        ),
        _ => descriptor(
            "unknown",
            "Unknown tool descriptor.",
            object_schema(json!({}), &[]),
        ),
    }
}

fn descriptor(
    name: &'static str,
    description: &'static str,
    input_schema: Value,
) -> McpToolDescriptor {
    McpToolDescriptor {
        name,
        description,
        input_schema,
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": true
    })
}

fn object_schema_with_required_aliases(
    properties: Value,
    required: &[&str],
    alias_groups: &[&[&str]],
) -> Value {
    let mut schema = object_schema(properties, required);
    if let Value::Object(object) = &mut schema {
        object.insert(
            "allOf".to_string(),
            Value::Array(
                alias_groups
                    .iter()
                    .map(|aliases| {
                        json!({
                            "anyOf": aliases
                                .iter()
                                .map(|alias| json!({ "required": [alias] }))
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect::<Vec<_>>(),
            ),
        );
    }
    schema
}
