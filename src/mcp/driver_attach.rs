use crate::adapters::tmux::CommandRunner;
use crate::driver::{
    DriverClient, DriverEndpointState, acquire_driver_attach_lock, cleanup_stale_driver_ipc,
    load_driver_recovery_state, private_driver_dir, probe_driver_endpoint,
    runtime_root_for_run_root,
};
use serde_json::{Value, json};

use super::driver_run_flow::{generate_driver_token, wait_for_driver_client, write_driver_token};
use super::{McpServer, ToolError};

pub(super) struct AttachedDriver {
    pub(super) client: DriverClient,
    pub(super) status: Value,
}

impl<R: CommandRunner> McpServer<R> {
    pub(super) fn attach_or_restart_driver(
        &mut self,
        run_id: &str,
    ) -> Result<Option<AttachedDriver>, ToolError> {
        let run_root = self
            .run_asset_store
            .run_root(run_id)
            .map_err(ToolError::from_run_asset)?;
        if !run_root.exists() {
            return Ok(None);
        }

        let _attach_lock = acquire_driver_attach_lock(&run_root).map_err(driver_recovery_error)?;
        let client = DriverClient::from_run_root_for_run(&run_root, run_id);
        if let Ok(Some(client)) = &client
            && let Ok(status) = client.request("status", run_id, &json!({}))
            && status.get("ok").and_then(Value::as_bool) == Some(true)
            && status.get("run_id").and_then(Value::as_str) == Some(run_id)
        {
            return Ok(Some(AttachedDriver {
                client: client.clone(),
                status,
            }));
        }
        if probe_driver_endpoint(&run_root).map_err(driver_recovery_error)?
            == DriverEndpointState::Accepting
        {
            return Err(ToolError::invalid(
                "runtime driver endpoint is live but failed its authenticated exact-run status probe",
            ));
        }
        let driver_dir = private_driver_dir(
            &runtime_root_for_run_root(&run_root).map_err(driver_recovery_error)?,
            &run_root,
        )
        .map_err(driver_recovery_error)?;
        if !driver_dir.join("events.jsonl").exists() {
            return Ok(None);
        }
        let recovery =
            load_driver_recovery_state(&run_root, run_id).map_err(driver_recovery_error)?;
        let Some(recovery) = recovery else {
            return Ok(None);
        };

        cleanup_stale_driver_ipc(&run_root, run_id).map_err(driver_recovery_error)?;
        if !self.tmux_adapter.supports_external_driver_launch() {
            return Err(ToolError::invalid(
                "runtime driver is unavailable and this MCP cannot launch its replacement",
            ));
        }
        let tmux = recovery.tmux.ok_or_else(|| {
            ToolError::invalid("durable runtime driver state has no tmux recovery context")
        })?;
        let runs_root = self
            .run_asset_store
            .runs_root()
            .map_err(ToolError::from_run_asset)?;
        let token = generate_driver_token()?;
        let token_path = write_driver_token(&run_root, &token)?;
        let driver = match self.launch_driver_pane_for_context(
            run_id,
            &tmux.session_id,
            &tmux.window_name,
            &run_root,
            &runs_root,
            &token_path,
        ) {
            Ok(driver) => driver,
            Err(err) => {
                let _ = cleanup_stale_driver_ipc(&run_root, run_id);
                return Err(err);
            }
        };
        let client = match wait_for_driver_client(&run_root, run_id) {
            Ok(client) => client,
            Err(err) => {
                self.cleanup_driver_startup(
                    &run_root,
                    &token_path,
                    run_id,
                    None,
                    Some(&driver),
                    None,
                )?;
                return Err(err);
            }
        };
        let status = client
            .request("status", run_id, &json!({}))
            .map_err(driver_recovery_error)?;
        if status.get("ok").and_then(Value::as_bool) != Some(true)
            || status.get("run_id").and_then(Value::as_str) != Some(run_id)
        {
            return Err(ToolError::invalid(
                "replacement runtime driver failed its exact-run status probe",
            ));
        }
        Ok(Some(AttachedDriver { client, status }))
    }
}

fn driver_recovery_error(err: std::io::Error) -> ToolError {
    ToolError::private_failure("runtime driver recovery is unavailable", err)
}
