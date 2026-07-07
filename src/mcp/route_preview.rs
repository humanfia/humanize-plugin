use crate::runtime;
use serde_json::{Value, json};

use super::{
    McpServerState, ToolCallResult, ToolError, content_hash, optional_string, require_string,
    run_not_found_guidance,
};

pub(super) fn preview_flow_routes(
    state: &McpServerState,
    arguments: &Value,
) -> Result<ToolCallResult, ToolError> {
    let run_id = require_string(arguments, &["run_id", "runId"])?;
    let requested_lock_id = optional_string(
        arguments,
        &["flow_lock_id", "flowLockId", "lock_id", "lockId"],
    )?;
    let provided_content_hash =
        optional_string(arguments, &["content_hash", "contentHash"])?.map(str::to_string);
    if !state.runtime.has_run(run_id) {
        if requested_lock_id.is_some() {
            return Ok(ToolCallResult::error(run_not_found_guidance(run_id)));
        }
        return Ok(ToolCallResult::error(json!({
            "ok": false,
            "run_id": run_id,
            "error": "flow_lock_id is required",
            "next_tool": "start_run",
            "next_arguments": {
                "run_id": run_id,
                "nodes": ["root"]
            },
            "after_next_tool": "apply_flow_lock"
        })));
    }

    let (lock_id, source, applied_content_hash) = match requested_lock_id {
        Some(lock_id) => (lock_id.to_string(), "explicit", None),
        None => {
            let runtime_state = state.runtime.state();
            let Some(application_id) = runtime_state
                .latest_flow_lock_application_by_run
                .get(run_id)
            else {
                return Ok(ToolCallResult::error(json!({
                    "ok": false,
                    "run_id": run_id,
                    "error": "flow_lock_id is required"
                })));
            };
            let Some(application) = runtime_state.flow_lock_applications.get(application_id) else {
                return Ok(ToolCallResult::error(json!({
                    "ok": false,
                    "run_id": run_id,
                    "error": "latest applied flow lock not found"
                })));
            };
            (
                application.lock_id.clone(),
                "latest_applied",
                Some(application.content_hash.clone()),
            )
        }
    };

    let Some(lock) = state.flow_locks.get(&lock_id) else {
        return Ok(ToolCallResult::error(json!({
            "ok": false,
            "run_id": run_id,
            "lock_id": lock_id,
            "flow_lock_id": lock_id,
            "content_hash": provided_content_hash.or(applied_content_hash),
            "expected_content_hash": Value::Null,
            "error": "flow lock not found"
        })));
    };
    let expected_content_hash = content_hash(lock.normalized_content());
    if let Some(applied_content_hash) = applied_content_hash.as_deref() {
        if applied_content_hash != expected_content_hash {
            return Ok(content_hash_mismatch(
                run_id,
                &lock_id,
                applied_content_hash,
                &expected_content_hash,
            ));
        }
    }
    if let Some(provided_content_hash) = provided_content_hash.as_deref() {
        if provided_content_hash != expected_content_hash {
            return Ok(content_hash_mismatch(
                run_id,
                &lock_id,
                provided_content_hash,
                &expected_content_hash,
            ));
        }
    }
    let response_content_hash = provided_content_hash
        .or(applied_content_hash)
        .unwrap_or_else(|| expected_content_hash.clone());

    let routes = runtime::preview_flow_routes(state.runtime.state(), run_id, lock)
        .map_err(ToolError::from_runtime)?;
    let routes = serde_json::to_value(routes)
        .map_err(|_| ToolError::invalid("route preview serialization failed"))?;

    Ok(ToolCallResult::ok(json!({
        "ok": true,
        "run_id": run_id,
        "flow_lock_id": lock_id,
        "lock_id": lock_id,
        "content_hash": response_content_hash,
        "source": source,
        "routes": routes
    })))
}

fn content_hash_mismatch(
    run_id: &str,
    lock_id: &str,
    content_hash: &str,
    expected_content_hash: &str,
) -> ToolCallResult {
    ToolCallResult::error(json!({
        "ok": false,
        "run_id": run_id,
        "lock_id": lock_id,
        "flow_lock_id": lock_id,
        "content_hash": content_hash,
        "expected_content_hash": expected_content_hash,
        "error": "flow lock content hash mismatch"
    }))
}

#[cfg(test)]
mod tests {
    use crate::flow::{
        FlowCheckMode, FlowDraft, FlowNode, FlowResource, FlowRoute, ResourceKind, flow_lock,
    };
    use crate::runtime::{FlowLockMode, NodeSpec};

    use super::*;

    fn preview_lock() -> crate::flow::FlowLock {
        flow_lock(
            &FlowDraft {
                nodes: vec![
                    FlowNode {
                        id: "root".into(),
                        ..FlowNode::default()
                    },
                    FlowNode {
                        id: "finish".into(),
                        ..FlowNode::default()
                    },
                ],
                resources: vec![FlowResource {
                    id: "readme.main".into(),
                    kind: ResourceKind::Readme,
                    source: "inline:Preview local routes.".into(),
                }],
                routes: vec![FlowRoute {
                    predicate: "exists(artifact.ready)".into(),
                    for_each: None,
                    activate: "finish".into(),
                }],
                ..FlowDraft::default()
            },
            FlowCheckMode::Core,
        )
        .unwrap()
    }

    #[test]
    fn preview_flow_routes_checks_latest_applied_hash_when_caller_hash_matches_lock() {
        let lock = preview_lock();
        let lock_id = lock.id().to_string();
        let expected_content_hash = content_hash(lock.normalized_content());
        let applied_content_hash = "fnv1a64:0000000000000000";
        let mut state = McpServerState::default();
        state.flow_locks.insert(lock_id.clone(), lock);
        state
            .runtime
            .start_run("run-applied-hash", vec![NodeSpec::new("root")])
            .unwrap();
        state
            .runtime
            .apply_flow_lock(
                "run-applied-hash",
                FlowLockMode::FutureActivations,
                lock_id.clone(),
                applied_content_hash,
            )
            .unwrap();

        let preview = preview_flow_routes(
            &state,
            &json!({
                "run_id": "run-applied-hash",
                "content_hash": expected_content_hash
            }),
        )
        .unwrap()
        .to_json();

        assert_eq!(preview["isError"], true);
        assert_eq!(preview["structuredContent"]["ok"], false);
        assert_eq!(preview["structuredContent"]["flow_lock_id"], lock_id);
        assert_eq!(preview["structuredContent"]["lock_id"], lock_id);
        assert_eq!(
            preview["structuredContent"]["content_hash"],
            applied_content_hash
        );
        assert_eq!(
            preview["structuredContent"]["expected_content_hash"],
            expected_content_hash
        );
        assert_eq!(
            preview["structuredContent"]["error"],
            "flow lock content hash mismatch"
        );
    }
}
