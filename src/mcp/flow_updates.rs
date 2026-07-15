use crate::adapters::tmux::CommandRunner;
use crate::{flow, runtime};
use serde_json::{Value, json};

use super::{
    McpServer, ProposedFlowUpdate, ToolCallResult, ToolError, diagnostics_json,
    flow_check_mode_arg, flow_draft_arg, flow_lock_mode_name, optional_flow_lock_mode_arg,
    optional_string,
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
        match flow::flow_lock(&draft, check_mode) {
            Ok(lock) => {
                let lock_id = lock.id().to_string();
                let content_hash = lock.content_hash().to_string();
                let diagnostics = diagnostics_json(lock.diagnostics());
                let revision_package =
                    super::driver_run_flow::flow_lock_package(&lock, &content_hash)?;
                self.state.flow_locks.insert(lock_id.clone(), lock);
                self.state.proposed_updates.insert(
                    lock_id.clone(),
                    ProposedFlowUpdate {
                        mode,
                        content_hash: content_hash.clone(),
                        summary: summary.clone(),
                    },
                );
                Ok(ToolCallResult::ok(json!({
                    "ok": true,
                    "risk": flow_update_risk(&diagnostics),
                    "apply_mode": flow_lock_mode_name(mode),
                    "flow_lock_id": lock_id,
                    "lock_id": lock_id,
                    "content_hash": content_hash,
                    "summary": summary,
                    "diagnostics": diagnostics,
                    "revision_package": revision_package
                })))
            }
            Err(err) => Ok(ToolCallResult::error(json!({
                "ok": false,
                "apply_mode": flow_lock_mode_name(mode),
                "summary": summary,
                "diagnostics": diagnostics_json(&err.diagnostics)
            }))),
        }
    }
}

fn flow_update_risk(diagnostics: &[Value]) -> &'static str {
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic["severity"] == "error")
    {
        "high"
    } else {
        "medium"
    }
}
