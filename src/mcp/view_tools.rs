use crate::adapters::tmux::CommandRunner;
use crate::view::{VisualizationSnapshot, render_terminal_dashboard, serve_browser_snapshot};
use serde_json::{Value, json};

use super::{McpServer, ToolCallResult, ToolError, optional_string, optional_u64};

impl<R: CommandRunner> McpServer<R> {
    pub(super) fn view_terminal(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let snapshot = self.view_snapshot_arg(arguments)?;
        let run_count = snapshot.runs.len();
        let dashboard = render_terminal_dashboard(&snapshot);

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "format": "terminal",
            "dashboard": dashboard,
            "run_count": run_count
        })))
    }

    pub(super) fn view_snapshot(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let snapshot = self.view_snapshot_arg(arguments)?;
        let run_count = snapshot.runs.len();

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "format": "json",
            "snapshot": snapshot,
            "run_count": run_count
        })))
    }

    pub(super) fn view_browser(&mut self, arguments: &Value) -> Result<ToolCallResult, ToolError> {
        let host = optional_string(arguments, &["host"])?.unwrap_or("127.0.0.1");
        let host = match host {
            "127.0.0.1" | "localhost" => "127.0.0.1",
            _ => {
                return Err(ToolError::invalid(
                    "view_browser host must be loopback: 127.0.0.1 or localhost",
                ));
            }
        };
        let port = optional_u64(arguments, &["port"])?
            .map(u16::try_from)
            .transpose()
            .map_err(|_| ToolError::invalid("port must be between 0 and 65535"))?
            .unwrap_or(0);
        let snapshot = self.state.runtime_snapshot();
        let run_count = snapshot.runs.len();
        let server = serve_browser_snapshot(host, port, &snapshot).map_err(ToolError::from_view)?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "url": server.url,
            "host": server.host,
            "port": server.port,
            "run_count": run_count
        })))
    }

    fn view_snapshot_arg(&self, arguments: &Value) -> Result<VisualizationSnapshot, ToolError> {
        let snapshot = self.state.runtime_snapshot();
        match optional_string(arguments, &["run_id", "runId"])? {
            Some(run_id) => {
                let Some(run) = snapshot.run(run_id) else {
                    return Err(ToolError::from_runtime(
                        crate::runtime::RuntimeError::RunNotFound {
                            run_id: run_id.to_string(),
                        },
                    ));
                };
                Ok(VisualizationSnapshot {
                    runs: vec![run.clone()],
                })
            }
            None => Ok(snapshot),
        }
    }
}
