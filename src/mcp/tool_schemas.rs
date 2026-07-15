use serde_json::{Value, json};

pub(super) fn start_run() -> Value {
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
            "qos": {
                "type": "object",
                "properties": {
                    "urgency": {
                        "type": "string",
                        "enum": ["interactive", "standard", "background"]
                    },
                    "completion_target": { "type": "string" },
                    "completionTarget": { "type": "string" }
                }
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
    )
}

pub(super) fn run_id() -> Value {
    object_schema_with_required_aliases(
        json!({
            "run_id": { "type": "string" },
            "runId": { "type": "string" }
        }),
        &[],
        &[&["run_id", "runId"]],
    )
}

pub(super) fn participant_context() -> Value {
    object_schema(json!({}), &[])
}

pub(super) fn deliver_artifact() -> Value {
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
    )
}

pub(super) fn participant_deliver_artifact() -> Value {
    object_schema_with_required_aliases(
        json!({
            "artifact_key": { "type": "string" },
            "artifactKey": { "type": "string" },
            "key": { "type": "string" },
            "payload": {}
        }),
        &[],
        &[&["artifact_key", "artifactKey", "key"]],
    )
}

pub(super) fn fanout_from_artifact() -> Value {
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
    )
}

pub(super) fn record_effect() -> Value {
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
    )
}

pub(super) fn participant_record_effect() -> Value {
    object_schema_with_required_aliases(
        json!({
            "effect_key": { "type": "string" },
            "effectKey": { "type": "string" },
            "key": { "type": "string" },
            "payload": {}
        }),
        &[],
        &[&["effect_key", "effectKey", "key"]],
    )
}

pub(super) fn record_hook_fact() -> Value {
    object_schema_with_required_aliases(
        json!({
            "run_id": { "type": "string" },
            "runId": { "type": "string" },
            "session_id": { "type": "string" },
            "sessionId": { "type": "string" },
            "activation_id": { "type": "string" },
            "activationId": { "type": "string" },
            "hook": {
                "type": "string",
                "description": "Native hook name such as compaction_pending or compaction_finished."
            },
            "source_native_id": { "type": "string" },
            "sourceNativeId": { "type": "string" },
            "causal_id": { "type": "string" },
            "causalId": { "type": "string" },
            "correlation_id": { "type": "string" },
            "correlationId": { "type": "string" },
            "payload": {}
        }),
        &["hook"],
        &[&["run_id", "runId"], &["session_id", "sessionId"]],
    )
}

pub(super) fn patch_board() -> Value {
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
    )
}

pub(super) fn activate_node() -> Value {
    object_schema_with_required_aliases(
        json!({
            "run_id": { "type": "string" },
            "runId": { "type": "string" },
            "node_id": { "type": "string" },
            "nodeId": { "type": "string" },
            "activation_id": { "type": "string" },
            "activationId": { "type": "string" },
            "for_each": { "type": "string" },
            "forEach": { "type": "string" },
            "required_artifacts": {
                "type": "array",
                "items": { "type": "string" }
            },
            "requiredArtifacts": {
                "type": "array",
                "items": { "type": "string" }
            },
            "required_effects": {
                "type": "array",
                "items": { "type": "string" }
            },
            "requiredEffects": {
                "type": "array",
                "items": { "type": "string" }
            }
        }),
        &[],
        &[&["run_id", "runId"], &["node_id", "nodeId"]],
    )
}

pub(super) fn send_message() -> Value {
    object_schema_with_required_aliases(
        json!({
            "run_id": { "type": "string" },
            "runId": { "type": "string" },
            "activation_id": { "type": "string" },
            "activationId": { "type": "string" },
            "message_id": { "type": "string", "minLength": 1 },
            "messageId": { "type": "string", "minLength": 1 },
            "text": { "type": "string", "minLength": 1 },
            "message": { "type": "string", "minLength": 1 }
        }),
        &[],
        &[
            &["run_id", "runId"],
            &["activation_id", "activationId"],
            &["message_id", "messageId"],
            &["text", "message"],
        ],
    )
}

pub(super) fn validate_stop() -> Value {
    object_schema_with_required_aliases(
        json!({
            "run_id": { "type": "string" },
            "runId": { "type": "string" },
            "activation_id": { "type": "string" },
            "activationId": { "type": "string" }
        }),
        &[],
        &[&["run_id", "runId"], &["activation_id", "activationId"]],
    )
}

pub(super) fn participant_validate_stop() -> Value {
    object_schema(json!({}), &[])
}

pub(super) fn observe_stop() -> Value {
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
    )
}

pub(super) fn apply_flow_lock() -> Value {
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
            "contentHash": { "type": "string" },
            "review_id": { "type": "string" },
            "reviewId": { "type": "string" }
        }),
        &["mode"],
        &[
            &["run_id", "runId"],
            &["lock_id", "lockId", "flow_lock_id", "flowLockId"],
            &["content_hash", "contentHash"],
            &["review_id", "reviewId"],
        ],
    )
}

pub(super) fn preview_flow_routes() -> Value {
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
    )
}

pub(super) fn run_flow() -> Value {
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
            "flow": { "type": "object" },
            "flow_lock_id": { "type": "string" },
            "flowLockId": { "type": "string" },
            "lock_id": { "type": "string" },
            "lockId": { "type": "string" },
            "content_hash": { "type": "string" },
            "contentHash": { "type": "string" },
            "flow_lock": { "type": "object" },
            "package_path": { "type": "string" },
            "packagePath": { "type": "string" },
            "run_mode": {
                "type": "string",
                "enum": ["finite", "continuous", "manual"]
            },
            "runMode": {
                "type": "string",
                "enum": ["finite", "continuous", "manual"]
            },
            "activation_limit": { "type": "integer", "minimum": 0 },
            "activationLimit": { "type": "integer", "minimum": 0 },
            "stop_attempt_limit": { "type": "integer", "minimum": 1, "maximum": 8 },
            "stopAttemptLimit": { "type": "integer", "minimum": 1, "maximum": 8 },
            "review_id": { "type": "string" },
            "reviewId": { "type": "string" },
            "qos": {
                "type": "object",
                "properties": {
                    "urgency": {
                        "type": "string",
                        "enum": ["interactive", "standard", "background"]
                    },
                    "completion_target": { "type": "string" },
                    "completionTarget": { "type": "string" }
                }
            },
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
        &[&["run_id", "runId"], &["review_id", "reviewId"]],
    )
}

pub(super) fn resume_run() -> Value {
    object_schema_with_required_aliases(
        json!({
            "run_id": { "type": "string" },
            "runId": { "type": "string" },
            "activation_limit": { "type": "integer", "minimum": 0 },
            "activationLimit": { "type": "integer", "minimum": 0 },
            "delivery_resolution": {
                "type": "object",
                "properties": {
                    "started_event_sequence": {
                        "type": "integer",
                        "minimum": 1
                    },
                    "outcome": {
                        "type": "string",
                        "enum": ["submitted", "not_submitted"]
                    },
                    "evidence": {
                        "type": "string",
                        "minLength": 1,
                        "pattern": ".*\\S.*"
                    }
                },
                "required": ["started_event_sequence", "outcome", "evidence"],
                "additionalProperties": false
            }
        }),
        &[],
        &[&["run_id", "runId"]],
    )
}

pub(super) fn view_browser() -> Value {
    object_schema(
        json!({
            "host": { "type": "string" },
            "port": { "type": "integer", "minimum": 0, "maximum": 65535 }
        }),
        &[],
    )
}

pub(super) fn flow_repair() -> Value {
    object_schema(
        json!({
            "flow": { "type": "object" },
            "mode": { "type": "string" },
            "include_warnings": { "type": "boolean" },
            "includeWarnings": { "type": "boolean" }
        }),
        &["flow"],
    )
}

pub(super) fn flow_apply() -> Value {
    object_schema(
        json!({
            "flow": { "type": "object" },
            "flow_lock": { "type": "object" },
            "package_path": { "type": "string" },
            "packagePath": { "type": "string" },
            "flow_lock_id": { "type": "string" },
            "flowLockId": { "type": "string" },
            "lock_id": { "type": "string" },
            "lockId": { "type": "string" }
        }),
        &[],
    )
}

pub(super) fn flow_suggest() -> Value {
    object_schema(
        json!({
            "goal": { "type": "string" },
            "readme": { "type": "string", "minLength": 1 },
            "nodes": {
                "type": "array",
                "items": { "type": "string" }
            },
            "artifact": { "type": "string" }
        }),
        &["goal", "readme"],
    )
}

pub(super) fn flow_draft() -> Value {
    object_schema(
        json!({
            "flow": { "type": "object" },
            "mode": { "type": "string" },
            "package_path": { "type": "string" },
            "packagePath": { "type": "string" }
        }),
        &["flow"],
    )
}

pub(super) fn flow_export() -> Value {
    object_schema_with_required_aliases(
        json!({
            "flow_lock_id": { "type": "string" },
            "flowLockId": { "type": "string" },
            "lock_id": { "type": "string" },
            "lockId": { "type": "string" },
            "flow_lock": { "type": "object" },
            "package_path": { "type": "string" },
            "packagePath": { "type": "string" },
            "format": { "type": "string", "enum": ["json", "yaml"] }
        }),
        &[],
        &[],
    )
}

pub(super) fn propose_flow_update() -> Value {
    object_schema(
        json!({
            "flow": { "type": "object" },
            "apply_mode": {
                "type": "string",
                "enum": ["future_activations", "checkpoint_restart"]
            },
            "applyMode": {
                "type": "string",
                "enum": ["future_activations", "checkpoint_restart"]
            },
            "summary": { "type": "string" }
        }),
        &["flow"],
    )
}

pub(super) fn apply_flow_update() -> Value {
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
            "review_id": { "type": "string" },
            "reviewId": { "type": "string" },
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
            &["review_id", "reviewId"],
        ],
    )
}

pub(super) fn prepare_flow_review() -> Value {
    object_schema(
        json!({
            "flow": { "type": "object" },
            "flow_lock_id": { "type": "string" },
            "flowLockId": { "type": "string" },
            "lock_id": { "type": "string" },
            "lockId": { "type": "string" },
            "content_hash": { "type": "string" },
            "contentHash": { "type": "string" },
            "flow_lock": { "type": "object" },
            "package_path": { "type": "string" },
            "packagePath": { "type": "string" },
            "title": { "type": "string" }
        }),
        &[],
    )
}

pub(super) fn decide_flow_review() -> Value {
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
    )
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
