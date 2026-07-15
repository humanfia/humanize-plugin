use crate::adapters::tmux::CommandRunner;
use crate::flow;
use serde_json::Value;
use std::path::Path;

use super::flow_json::diagnostic_codes_text;
use super::{McpServer, ToolError, flow_check_mode_arg, flow_draft_arg, optional_string};

impl<R: CommandRunner> McpServer<R> {
    pub(super) fn flow_lock_binding_from_arguments(
        &mut self,
        arguments: &Value,
    ) -> Result<Option<(String, String)>, ToolError> {
        if let Some(value) = arguments.get("flow_lock") {
            let lock = serde_json::from_value::<flow::FlowLock>(value.clone())
                .map_err(|error| ToolError::invalid(format!("invalid flow lock: {error}")))?;
            return self.cache_flow_lock(arguments, lock).map(Some);
        }
        if let Some(package_path) = optional_string(
            arguments,
            &[
                "package_path",
                "packagePath",
                "flow_lock_path",
                "flowLockPath",
            ],
        )? {
            let lock = flow::FlowLock::load_directory(Path::new(package_path))
                .map_err(|error| ToolError::invalid(format!("flow lock load failed: {error}")))?;
            return self.cache_flow_lock(arguments, lock).map(Some);
        }
        if arguments.get("flow").is_some() {
            return self.lock_flow_from_arguments(arguments).map(Some);
        }
        let Some(lock_id) = optional_string(
            arguments,
            &["flow_lock_id", "flowLockId", "lock_id", "lockId"],
        )?
        else {
            return Ok(None);
        };
        self.validate_flow_lock_binding(arguments, lock_id)
            .map(|content_hash| Some((lock_id.to_string(), content_hash)))
    }

    pub(super) fn require_flow_lock_binding_from_arguments(
        &mut self,
        arguments: &Value,
    ) -> Result<(String, String), ToolError> {
        self.flow_lock_binding_from_arguments(arguments)?
            .ok_or_else(|| ToolError::missing("flow_lock_id"))
    }

    fn lock_flow_from_arguments(
        &mut self,
        arguments: &Value,
    ) -> Result<(String, String), ToolError> {
        let draft = flow_draft_arg(arguments)?;
        let mode = flow_check_mode_arg(arguments)?;
        match flow::flow_lock(&draft, mode) {
            Ok(lock) => {
                let lock_id = lock.id().to_string();
                let content_hash = lock.content_hash().to_string();
                self.state.flow_locks.insert(lock_id.clone(), lock);
                Ok((lock_id, content_hash))
            }
            Err(err) => Err(ToolError::invalid(format!(
                "flow lock failed: {}",
                diagnostic_codes_text(&err.diagnostics)
            ))),
        }
    }

    fn validate_flow_lock_binding(
        &self,
        arguments: &Value,
        lock_id: &str,
    ) -> Result<String, ToolError> {
        let Some(lock) = self.state.flow_locks.get(lock_id) else {
            return Err(ToolError::invalid("flow lock not found"));
        };
        let expected_content_hash = lock.content_hash().to_string();
        if let Some(provided_content_hash) =
            optional_string(arguments, &["content_hash", "contentHash"])?
            && provided_content_hash != expected_content_hash
        {
            return Err(ToolError::invalid("flow lock content hash mismatch"));
        }
        Ok(expected_content_hash)
    }

    fn cache_flow_lock(
        &mut self,
        arguments: &Value,
        lock: flow::FlowLock,
    ) -> Result<(String, String), ToolError> {
        if let Some(provided_lock_id) = optional_string(
            arguments,
            &["flow_lock_id", "flowLockId", "lock_id", "lockId"],
        )? && provided_lock_id != lock.id()
        {
            return Err(ToolError::invalid("flow lock id mismatch"));
        }
        if let Some(provided_hash) = optional_string(arguments, &["content_hash", "contentHash"])?
            && provided_hash != lock.content_hash()
        {
            return Err(ToolError::invalid("flow lock content hash mismatch"));
        }
        let lock_id = lock.id().to_string();
        let content_hash = lock.content_hash().to_string();
        self.state.flow_locks.insert(lock_id.clone(), lock);
        Ok((lock_id, content_hash))
    }
}
