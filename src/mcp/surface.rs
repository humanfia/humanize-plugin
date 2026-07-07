use serde::Serialize;
use serde_json::{Value, json};

pub const RUNTIME_TOOL_NAMES: [&str; 14] = [
    "start_run",
    "get_context",
    "deliver_artifact",
    "fanout_from_artifact",
    "record_effect",
    "patch_board",
    "activate_node",
    "send_message",
    "validate_stop",
    "apply_flow_lock",
    "preview_flow_routes",
    "view_terminal",
    "view_snapshot",
    "view_browser",
];

pub const AUTHORING_TOOL_NAMES: [&str; 5] = [
    "flow_apply",
    "flow_suggest",
    "flow_check",
    "flow_lock",
    "flow_export",
];

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

    pub fn tools(&self) -> Vec<McpToolDescriptor> {
        RUNTIME_TOOL_NAMES
            .iter()
            .chain(AUTHORING_TOOL_NAMES.iter())
            .map(|name| descriptor_for(name))
            .collect()
    }

    pub fn lookup(&self, name: &str) -> Option<McpToolDescriptor> {
        RUNTIME_TOOL_NAMES
            .iter()
            .chain(AUTHORING_TOOL_NAMES.iter())
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
            "Start a local workflow run and create initial node activations.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
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
                &["run_id"],
            ),
        ),
        "get_context" => descriptor(
            "get_context",
            "Return local context for one run or all in-memory runs.",
            object_schema(json!({ "run_id": { "type": "string" } }), &[]),
        ),
        "deliver_artifact" => descriptor(
            "deliver_artifact",
            "Record an artifact payload for a run activation.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "activation_id": { "type": "string" },
                    "artifact_key": { "type": "string" },
                    "payload": {}
                }),
                &["run_id", "activation_id", "artifact_key"],
            ),
        ),
        "fanout_from_artifact" => descriptor(
            "fanout_from_artifact",
            "Create one runtime activation per line in the latest artifact slot.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "node_id": { "type": "string" },
                    "artifact_key": { "type": "string" },
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
                &["run_id", "node_id", "artifact_key"],
            ),
        ),
        "record_effect" => descriptor(
            "record_effect",
            "Record an effect fact for a run activation.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "activation_id": { "type": "string" },
                    "effect_key": { "type": "string" },
                    "payload": {}
                }),
                &["run_id", "activation_id", "effect_key"],
            ),
        ),
        "patch_board" => descriptor(
            "patch_board",
            "Patch local run board values with optional version checking.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "activation_id": { "type": "string" },
                    "expected_version": { "type": "integer", "minimum": 0 },
                    "patch": { "type": "object" }
                }),
                &["run_id", "activation_id", "patch"],
            ),
        ),
        "activate_node" => descriptor(
            "activate_node",
            "Create runtime metadata for a node activation.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "node_id": { "type": "string" },
                    "activation_id": { "type": "string" }
                }),
                &["run_id", "node_id"],
            ),
        ),
        "send_message" => descriptor(
            "send_message",
            "Store a local message associated with a run.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "message": {}
                }),
                &["run_id", "message"],
            ),
        ),
        "validate_stop" => descriptor(
            "validate_stop",
            "Validate whether a runtime activation satisfies its local stop contract.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "activation_id": { "type": "string" }
                }),
                &["run_id", "activation_id"],
            ),
        ),
        "apply_flow_lock" => descriptor(
            "apply_flow_lock",
            "Apply a flow lock to runtime policy with lock provenance.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "mode": {
                        "type": "string",
                        "enum": ["future_activations", "checkpoint_restart"]
                    },
                    "lock_id": { "type": "string" },
                    "content_hash": { "type": "string" }
                }),
                &["run_id", "mode", "lock_id", "content_hash"],
            ),
        ),
        "preview_flow_routes" => descriptor(
            "preview_flow_routes",
            "Preview typed flow lock route activations from current runtime facts.",
            object_schema(
                json!({
                    "run_id": { "type": "string" },
                    "flow_lock_id": { "type": "string" },
                    "flowLockId": { "type": "string" },
                    "lock_id": { "type": "string" },
                    "lockId": { "type": "string" },
                    "content_hash": { "type": "string" },
                    "contentHash": { "type": "string" }
                }),
                &["run_id"],
            ),
        ),
        "view_terminal" => descriptor(
            "view_terminal",
            "Render the current in-memory runtime snapshot as terminal text.",
            object_schema(json!({ "run_id": { "type": "string" } }), &[]),
        ),
        "view_snapshot" => descriptor(
            "view_snapshot",
            "Return the current in-memory runtime snapshot as structured JSON.",
            object_schema(json!({ "run_id": { "type": "string" } }), &[]),
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
        "flow_apply" => descriptor(
            "flow_apply",
            "Record that a supplied or locked flow was selected for application.",
            object_schema(
                json!({ "flow": {}, "flow_lock_id": { "type": "string" } }),
                &[],
            ),
        ),
        "flow_suggest" => descriptor(
            "flow_suggest",
            "Suggest a minimal flow draft skeleton for a terse authoring goal.",
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
            "Run the flow authoring checker for a flow draft.",
            object_schema(
                json!({ "flow": {}, "mode": { "type": "string" } }),
                &["flow"],
            ),
        ),
        "flow_lock" => descriptor(
            "flow_lock",
            "Create a deterministic flow lock for a valid flow draft.",
            object_schema(
                json!({ "flow": {}, "mode": { "type": "string" } }),
                &["flow"],
            ),
        ),
        "flow_export" => descriptor(
            "flow_export",
            "Export a known flow lock through the flow authoring exporter.",
            object_schema(
                json!({
                    "flow_lock_id": { "type": "string" },
                    "format": { "type": "string", "enum": ["json", "yaml"] }
                }),
                &["flow_lock_id"],
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
