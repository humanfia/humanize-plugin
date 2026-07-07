use crate::adapters::tmux::CommandRunner;
use crate::{flow, runtime};
use serde_json::{Value, json};

use super::{
    FlowReviewStatus, McpServer, ProposedFlowUpdate, ToolCallResult, ToolError, content_hash,
    diagnostics_json, flow_check_mode_arg, flow_draft_arg, flow_lock_mode_name, optional_bool,
    optional_flow_lock_mode_arg, optional_string, require_string, run_not_found_guidance,
};

impl<R: CommandRunner> McpServer<R> {
    pub(super) fn propose_flow_update(
        &mut self,
        arguments: &Value,
    ) -> Result<ToolCallResult, ToolError> {
        let draft = flow_draft_arg(arguments)?;
        let mode = optional_flow_lock_mode_arg(arguments, &["apply_mode", "applyMode"])?
            .unwrap_or(runtime::FlowLockMode::FutureActivations);
        let check_mode = flow_check_mode_arg(arguments)?;
        let summary = optional_string(arguments, &["summary"])?
            .unwrap_or("Flow update proposal.")
            .to_string();
        let review_required =
            optional_bool(arguments, &["review_required", "reviewRequired"])?.unwrap_or(true);
        match flow::flow_lock(&draft, check_mode) {
            Ok(lock) => {
                let lock_id = lock.id().to_string();
                let content_hash = content_hash(lock.normalized_content());
                let diagnostics = diagnostics_json(lock.diagnostics());
                self.state.flow_locks.insert(lock_id.clone(), lock);
                self.state.proposed_updates.insert(
                    lock_id.clone(),
                    ProposedFlowUpdate {
                        mode,
                        content_hash: content_hash.clone(),
                        summary: summary.clone(),
                        review_required,
                    },
                );
                Ok(ToolCallResult::ok(json!({
                    "ok": true,
                    "risk": flow_update_risk(&diagnostics, review_required),
                    "apply_mode": flow_lock_mode_name(mode),
                    "review_required": review_required,
                    "flow_lock_id": lock_id,
                    "lock_id": lock_id,
                    "content_hash": content_hash,
                    "summary": summary,
                    "diagnostics": diagnostics
                })))
            }
            Err(err) => Ok(ToolCallResult::error(json!({
                "ok": false,
                "apply_mode": flow_lock_mode_name(mode),
                "review_required": review_required,
                "summary": summary,
                "diagnostics": diagnostics_json(&err.diagnostics)
            }))),
        }
    }

    pub(super) fn apply_flow_update(
        &mut self,
        arguments: &Value,
    ) -> Result<ToolCallResult, ToolError> {
        let run_id = require_string(arguments, &["run_id", "runId"])?;
        let lock_id = require_string(
            arguments,
            &["flow_lock_id", "flowLockId", "lock_id", "lockId"],
        )?;
        let provided_content_hash = require_string(arguments, &["content_hash", "contentHash"])?;
        let Some(lock) = self.state.flow_locks.get(lock_id) else {
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "flow_lock_id": lock_id,
                "lock_id": lock_id,
                "error": "flow lock not found"
            })));
        };
        let expected_content_hash = content_hash(lock.normalized_content());
        if provided_content_hash != expected_content_hash {
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "run_id": run_id,
                "flow_lock_id": lock_id,
                "lock_id": lock_id,
                "content_hash": provided_content_hash,
                "expected_content_hash": expected_content_hash,
                "error": "flow lock content hash mismatch"
            })));
        }
        if !self.state.runtime().has_run(run_id) {
            return Ok(ToolCallResult::error(run_not_found_guidance(run_id)));
        }

        let proposed = self.state.proposed_updates.get(lock_id).map(|proposal| {
            (
                proposal.mode,
                proposal.content_hash.clone(),
                proposal.summary.clone(),
                proposal.review_required,
            )
        });
        if let Some((_, proposal_hash, _, true)) = proposed.as_ref() {
            match self.review_status_for_lock(lock_id) {
                Some(FlowReviewStatus::Approved | FlowReviewStatus::Bypassed) => {}
                Some(FlowReviewStatus::Rejected) => {
                    return Ok(ToolCallResult::error(json!({
                        "ok": false,
                        "run_id": run_id,
                        "flow_lock_id": lock_id,
                        "lock_id": lock_id,
                        "content_hash": proposal_hash,
                        "review_required": true,
                        "review_status": "rejected",
                        "error": "flow update review rejected",
                        "next_tool": "prepare_flow_review",
                        "after_next_tool": "approve_flow_review"
                    })));
                }
                Some(FlowReviewStatus::Pending) | None => {
                    return Ok(ToolCallResult::error(json!({
                        "ok": false,
                        "run_id": run_id,
                        "flow_lock_id": lock_id,
                        "lock_id": lock_id,
                        "content_hash": proposal_hash,
                        "review_required": true,
                        "review_status": self
                            .review_status_for_lock(lock_id)
                            .map(FlowReviewStatus::as_str)
                            .unwrap_or("missing"),
                        "error": "flow update review required",
                        "next_tool": "prepare_flow_review",
                        "after_next_tool": "approve_flow_review"
                    })));
                }
            }
        }
        let mode = match optional_flow_lock_mode_arg(arguments, &["apply_mode", "applyMode"])? {
            Some(mode) => mode,
            None => proposed
                .as_ref()
                .map(|proposal| proposal.0)
                .unwrap_or(runtime::FlowLockMode::FutureActivations),
        };
        self.state
            .runtime_mut()
            .apply_flow_lock(run_id, mode, lock_id, provided_content_hash)
            .map_err(ToolError::from_runtime)?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "applied": true,
            "run_id": run_id,
            "apply_mode": flow_lock_mode_name(mode),
            "flow_lock_id": lock_id,
            "lock_id": lock_id,
            "content_hash": provided_content_hash,
            "summary": proposed.as_ref().map(|proposal| proposal.2.as_str()),
            "review_required": proposed.as_ref().map(|proposal| proposal.3).unwrap_or(false)
        })))
    }
}

fn flow_update_risk(diagnostics: &[Value], review_required: bool) -> &'static str {
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic["severity"] == "error")
    {
        "high"
    } else if review_required {
        "medium"
    } else {
        "low"
    }
}
