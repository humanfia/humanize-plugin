use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, Read, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use serde::Serialize;
use serde_json::{Value, json};

use crate::runtime::{self, RuntimeState};

mod review;

pub use review::{
    AdapterCapabilityReview, DiffEntry, FlowGraph, FlowGraphEdge, FlowGraphNode,
    FlowReviewContract, FlowReviewNode, FlowReviewRoute, FlowReviewSnapshot, FlowValueFlow,
    FlowVisualDiff, ReviewRisk, render_flow_review_document,
};

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct VisualizationSnapshot {
    pub runs: Vec<RunSnapshot>,
}

impl VisualizationSnapshot {
    pub fn from_runtime(state: &RuntimeState, message_counts: &BTreeMap<String, usize>) -> Self {
        let runs = state
            .runs
            .iter()
            .map(|run_id| RunSnapshot::from_runtime(state, run_id, message_counts))
            .collect();

        Self { runs }
    }

    pub fn run(&self, run_id: &str) -> Option<&RunSnapshot> {
        self.runs.iter().find(|run| run.run_id == run_id)
    }

    pub fn run_mut(&mut self, run_id: &str) -> Option<&mut RunSnapshot> {
        self.runs.iter_mut().find(|run| run.run_id == run_id)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RunSnapshot {
    pub run_id: String,
    pub run_status: String,
    pub driver_mode: String,
    pub driver_mode_detail: String,
    pub activation_count: usize,
    pub artifact_count: usize,
    pub effect_count: usize,
    pub board_version: u64,
    pub message_count: usize,
    pub missing_stop_contract_count: usize,
    pub activation_ids: Vec<String>,
    pub activations: BTreeMap<String, ActivationSnapshot>,
    pub artifacts: BTreeMap<String, ArtifactSnapshot>,
    pub latest_artifact_by_slot_index: BTreeMap<String, String>,
    pub effects: BTreeMap<String, String>,
    pub board: BTreeMap<String, String>,
    pub flow_lock_mode: Option<String>,
    pub flow_lock_id: Option<String>,
    pub content_hash: Option<String>,
    pub flow_review_status: Option<String>,
    pub flow_export_document: Option<String>,
    pub latest_flow_lock_application: Option<String>,
    pub flow_lock_applications: BTreeMap<String, FlowLockApplicationSnapshot>,
    pub missing_stop_contracts: BTreeMap<String, Vec<String>>,
    pub runtime_budgets: Vec<RuntimeBudgetSnapshot>,
    pub pane_mappings: Vec<PaneMappingSnapshot>,
    pub event_count: usize,
    pub event_timeline: Vec<RuntimeEventSnapshot>,
    pub last_decision: Option<RuntimeDecisionSnapshot>,
    pub stop_decisions: Vec<RuntimeStopDecisionSnapshot>,
    pub machine_inputs: Vec<Value>,
    pub actuation_warnings: Vec<Value>,
    pub waiting_human: Vec<Value>,
    pub why: Option<String>,
}

impl RunSnapshot {
    fn from_runtime(
        state: &RuntimeState,
        run_id: &str,
        message_counts: &BTreeMap<String, usize>,
    ) -> Self {
        let activations = state
            .activations
            .iter()
            .filter(|((activation_run_id, _), _)| activation_run_id == run_id)
            .map(|((_, activation_id), activation)| {
                (
                    activation_id.clone(),
                    ActivationSnapshot::from_runtime(state, activation),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let activation_ids = activations.keys().cloned().collect::<Vec<_>>();
        let artifacts = state
            .artifact_records
            .iter()
            .filter(|(_, artifact)| artifact.run_id == run_id)
            .map(|(artifact_id, artifact)| {
                (
                    artifact_id.clone(),
                    ArtifactSnapshot {
                        artifact_id: artifact.artifact_id.clone(),
                        run_id: artifact.run_id.clone(),
                        activation_id: artifact.activation_id.clone(),
                        artifact_key: artifact.artifact_key.clone(),
                        content_hash: artifact.content_hash.clone(),
                        payload: artifact.payload.clone(),
                        event_sequence: artifact.event_sequence,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let latest_artifact_by_slot_index = state
            .latest_artifact_by_slot_index
            .iter()
            .filter(|((slot_run_id, _), _)| slot_run_id == run_id)
            .map(|((_, artifact_key), artifact_id)| (artifact_key.clone(), artifact_id.clone()))
            .collect::<BTreeMap<_, _>>();
        let effects = state
            .effects
            .iter()
            .filter(|((effect_run_id, _, _), _)| effect_run_id == run_id)
            .map(|((_, activation_id, effect_key), payload)| {
                (format!("{activation_id}:{effect_key}"), payload.clone())
            })
            .collect::<BTreeMap<_, _>>();
        let flow_lock_applications = state
            .flow_lock_applications
            .iter()
            .filter(|(_, application)| application.run_id == run_id)
            .map(|(application_id, application)| {
                (
                    application_id.clone(),
                    FlowLockApplicationSnapshot {
                        application_id: application.application_id.clone(),
                        run_id: application.run_id.clone(),
                        mode: flow_lock_mode_name(application.mode).to_string(),
                        lock_id: application.lock_id.clone(),
                        content_hash: application.content_hash.clone(),
                        event_sequence: application.event_sequence,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let missing_stop_contracts = activations
            .iter()
            .map(|(activation_id, activation)| {
                (
                    activation_id.clone(),
                    activation.missing_stop_contract.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let missing_stop_contract_count =
            missing_stop_contracts.values().map(Vec::len).sum::<usize>();
        let run_status = if activations
            .values()
            .any(|activation| activation.status == "running")
        {
            "running"
        } else {
            "stopped"
        };
        let run_status = state
            .run_statuses
            .get(run_id)
            .copied()
            .map(run_status_name)
            .unwrap_or(run_status);

        Self {
            run_id: run_id.to_string(),
            run_status: run_status.to_string(),
            driver_mode: "event_driven_mcp".to_string(),
            driver_mode_detail: "progress advances on MCP tool calls; no background daemon is attached to this in-process driver".to_string(),
            activation_count: activations.len(),
            artifact_count: artifacts.len(),
            effect_count: effects.len(),
            board_version: state.board_versions.get(run_id).copied().unwrap_or(0),
            message_count: message_counts.get(run_id).copied().unwrap_or(0),
            missing_stop_contract_count,
            activation_ids,
            activations,
            artifacts,
            latest_artifact_by_slot_index,
            effects,
            board: state.boards.get(run_id).cloned().unwrap_or_default(),
            flow_lock_mode: state
                .flow_lock_mode_by_run
                .get(run_id)
                .copied()
                .map(flow_lock_mode_name)
                .map(str::to_string),
            flow_lock_id: state.flow_lock_id_by_run.get(run_id).cloned(),
            content_hash: state.contract_hash_by_run.get(run_id).cloned(),
            flow_review_status: None,
            flow_export_document: None,
            latest_flow_lock_application: state
                .latest_flow_lock_application_by_run
                .get(run_id)
                .cloned(),
            flow_lock_applications,
            missing_stop_contracts,
            runtime_budgets: Vec::new(),
            pane_mappings: Vec::new(),
            event_count: 0,
            event_timeline: Vec::new(),
            last_decision: None,
            stop_decisions: Vec::new(),
            machine_inputs: Vec::new(),
            actuation_warnings: Vec::new(),
            waiting_human: Vec::new(),
            why: None,
        }
    }

    pub fn to_context_json(&self) -> Value {
        let activations = self
            .activations
            .iter()
            .map(|(activation_id, activation)| {
                (activation_id.clone(), activation.to_context_json())
            })
            .collect::<BTreeMap<_, _>>();

        json!({
            "run_id": self.run_id,
            "activation_ids": self.activation_ids,
            "activations": activations,
            "artifacts": self.artifacts,
            "latest_artifact_by_slot_index": self.latest_artifact_by_slot_index,
            "effects": self.effects,
            "board": self.board,
            "board_version": self.board_version,
            "message_count": self.message_count,
            "flow_lock_mode": self.flow_lock_mode,
            "flow_lock_id": self.flow_lock_id,
            "content_hash": self.content_hash,
            "flow_review_status": self.flow_review_status,
            "flow_export_document": self.flow_export_document,
            "latest_flow_lock_application": self.latest_flow_lock_application,
            "flow_lock_applications": self.flow_lock_applications,
            "missing_stop_contract_count": self.missing_stop_contract_count,
            "missing_stop_contracts": self.missing_stop_contracts,
            "run_status": self.run_status,
            "driver_mode": self.driver_mode,
            "driver_mode_detail": self.driver_mode_detail,
            "runtime_budgets": self.runtime_budgets,
            "pane_mappings": self.pane_mappings,
            "event_count": self.event_count,
            "event_timeline": self.event_timeline,
            "last_decision": self.last_decision,
            "stop_decisions": self.stop_decisions,
            "machine_inputs": self.machine_inputs,
            "actuation_warnings": self.actuation_warnings,
            "waiting_human": self.waiting_human,
            "why": self.why
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RuntimeBudgetSnapshot {
    pub name: String,
    pub used: u64,
    pub limit: u64,
    pub unit: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct PaneMappingSnapshot {
    pub activation_id: String,
    pub run_id: String,
    pub pane: String,
    pub session_id: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RuntimeEventSnapshot {
    pub sequence: u64,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RuntimeDecisionSnapshot {
    pub decision_id: String,
    pub summary: String,
    pub why: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RuntimeStopDecisionSnapshot {
    pub decision_id: String,
    pub activation_id: String,
    pub decision: String,
    pub attempt: u32,
    pub reason: Option<String>,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ActivationSnapshot {
    pub activation_id: String,
    pub run_id: String,
    pub node_id: String,
    pub stable_key: Option<String>,
    pub status: String,
    pub context: BTreeMap<String, String>,
    pub required_artifacts: Vec<String>,
    pub required_effects: Vec<String>,
    pub flow_lock_mode: Option<String>,
    pub flow_lock_id: Option<String>,
    pub contract_hash: Option<String>,
    pub missing_stop_contract: Vec<String>,
    pub pane: Option<PaneMappingSnapshot>,
}

impl ActivationSnapshot {
    fn from_runtime(state: &RuntimeState, activation: &runtime::Activation) -> Self {
        let required_artifacts = activation.stop_contract.required_artifacts().to_vec();
        let required_effects = activation.stop_contract.required_effects().to_vec();
        let missing_stop_contract = missing_stop_contract(state, activation);

        Self {
            activation_id: activation.activation_id.clone(),
            run_id: activation.run_id.clone(),
            node_id: activation.node_id.clone(),
            stable_key: activation.stable_key.clone(),
            status: activation_status_name(activation.status).to_string(),
            context: activation.context.clone(),
            required_artifacts,
            required_effects,
            flow_lock_mode: activation
                .flow_lock_mode
                .map(flow_lock_mode_name)
                .map(str::to_string),
            flow_lock_id: activation.flow_lock_id.clone(),
            contract_hash: activation.contract_hash.clone(),
            missing_stop_contract,
            pane: None,
        }
    }

    fn to_context_json(&self) -> Value {
        json!({
            "activation_id": self.activation_id,
            "run_id": self.run_id,
            "node_id": self.node_id,
            "stable_key": self.stable_key,
            "status": self.status,
            "context": self.context,
            "required_artifacts": self.required_artifacts,
            "required_effects": self.required_effects,
            "flow_lock_mode": self.flow_lock_mode,
            "flow_lock_id": self.flow_lock_id,
            "contract_hash": self.contract_hash,
            "pane": self.pane
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ArtifactSnapshot {
    pub artifact_id: String,
    pub run_id: String,
    pub activation_id: String,
    pub artifact_key: String,
    pub content_hash: String,
    pub payload: String,
    pub event_sequence: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct FlowLockApplicationSnapshot {
    pub application_id: String,
    pub run_id: String,
    pub mode: String,
    pub lock_id: String,
    pub content_hash: String,
    pub event_sequence: u64,
}

pub fn render_terminal_dashboard(snapshot: &VisualizationSnapshot) -> String {
    let mut output = String::new();
    writeln!(output, "humanize dashboard").expect("writing to a string should not fail");
    writeln!(output, "runs {}", snapshot.runs.len()).expect("writing to a string should not fail");

    for run in &snapshot.runs {
        let pane_count = if run.pane_mappings.is_empty() {
            "none".to_string()
        } else {
            run.pane_mappings.len().to_string()
        };
        writeln!(
            output,
            "run {} | activations {} | board version {} | messages {} | artifacts {} | effects {} | missing {} | status {} | panes {}",
            run.run_id,
            run.activation_count,
            run.board_version,
            run.message_count,
            run.artifact_count,
            run.effect_count,
            run.missing_stop_contract_count,
            run.run_status,
            pane_count
        )
        .expect("writing to a string should not fail");

        if let Some(why) = &run.why {
            writeln!(output, "  why {why}").expect("writing to a string should not fail");
        }

        if let Some(decision) = &run.last_decision {
            writeln!(
                output,
                "  last decision {} | {} | why {}",
                decision.decision_id, decision.summary, decision.why
            )
            .expect("writing to a string should not fail");
        }

        for event in &run.event_timeline {
            writeln!(
                output,
                "  event {} | {} | {}",
                event.sequence, event.label, event.detail
            )
            .expect("writing to a string should not fail");
        }

        for pane in &run.pane_mappings {
            writeln!(
                output,
                "  pane {} | {} | {}",
                pane.activation_id, pane.pane, pane.status
            )
            .expect("writing to a string should not fail");
        }

        for budget in &run.runtime_budgets {
            writeln!(
                output,
                "  budget {} | {}/{} {}",
                budget.name, budget.used, budget.limit, budget.unit
            )
            .expect("writing to a string should not fail");
        }

        for activation_id in &run.activation_ids {
            let activation = run
                .activations
                .get(activation_id)
                .expect("activation ids should match activation map");
            let missing = if activation.missing_stop_contract.is_empty() {
                "none".to_string()
            } else {
                activation.missing_stop_contract.join(", ")
            };
            let pane = activation
                .pane
                .as_ref()
                .map(|pane| pane.pane.as_str())
                .unwrap_or("none");
            writeln!(
                output,
                "  {} | node {} | missing {} | status {} | pane {}",
                activation.activation_id, activation.node_id, missing, activation.status, pane
            )
            .expect("writing to a string should not fail");
        }
    }

    output
}

pub fn snapshot_json(snapshot: &VisualizationSnapshot) -> serde_json::Result<String> {
    serde_json::to_string(snapshot)
}

pub fn render_browser_document(snapshot: &VisualizationSnapshot) -> serde_json::Result<String> {
    let json = snapshot_json(snapshot)?;
    let bootstrap_json = escape_script_json(&json);
    let body = render_browser_body(snapshot);

    Ok(format!(
        concat!(
            "<!doctype html>\n",
            "<html lang=\"en\">\n",
            "<head>\n",
            "<meta charset=\"utf-8\">\n",
            "<title>Humanize Dashboard</title>\n",
            "<style>",
            "body{{font-family:system-ui,sans-serif;margin:24px;color:#172026;background:#f7f8f5;}}",
            "main{{max-width:960px;margin:0 auto;}}",
            "section{{border:1px solid #cfd6cc;border-radius:8px;background:#fff;padding:16px;margin:16px 0;}}",
            "h1,h2{{margin:0 0 12px;}}",
            "table{{border-collapse:collapse;width:100%;}}",
            "th,td{{border-top:1px solid #e0e4dd;padding:8px;text-align:left;}}",
            "code{{font-family:ui-monospace,monospace;}}",
            "</style>\n",
            "</head>\n",
            "<body>\n",
            "<main>\n",
            "<h1>Humanize Dashboard</h1>\n",
            "{}",
            "</main>\n",
            "<script type=\"application/json\" id=\"humanize-view-snapshot\">{}</script>\n",
            "</body>\n",
            "</html>\n"
        ),
        body, bootstrap_json
    ))
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct BrowserViewServer {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) url: String,
}

pub(crate) fn serve_browser_snapshot(
    host: &str,
    port: u16,
    snapshot: &VisualizationSnapshot,
) -> io::Result<BrowserViewServer> {
    let html = render_browser_document(snapshot).map_err(io_other)?;
    let snapshot_json = snapshot_json(snapshot).map_err(io_other)?;
    let listener = TcpListener::bind((host, port))?;
    let local_addr = listener.local_addr()?;
    let local_host = local_addr.ip().to_string();
    let url_host = if local_addr.ip().is_ipv6() {
        format!("[{}]", local_addr.ip())
    } else {
        local_host.clone()
    };
    let port = local_addr.port();
    let html = Arc::new(html);
    let snapshot_json = Arc::new(snapshot_json);

    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = serve_browser_stream(stream, html.as_str(), snapshot_json.as_str());
        }
    });

    Ok(BrowserViewServer {
        host: local_host,
        port,
        url: format!("http://{url_host}:{port}/"),
    })
}

fn render_browser_body(snapshot: &VisualizationSnapshot) -> String {
    let mut body = String::new();
    writeln!(body, "<p>Runs: {}</p>", snapshot.runs.len())
        .expect("writing to a string should not fail");

    for run in &snapshot.runs {
        write!(
            body,
            concat!(
                "<section>",
                "<h2><code>{}</code></h2>",
                "<p>Activations: {} | Board: v{} | Messages: {} | Artifacts: {} | Effects: {} | Missing: {}</p>",
                "<table><thead><tr><th>Activation</th><th>Node</th><th>Missing</th></tr></thead><tbody>"
            ),
            escape_html(&run.run_id),
            run.activation_count,
            run.board_version,
            run.message_count,
            run.artifact_count,
            run.effect_count,
            run.missing_stop_contract_count
        )
        .expect("writing to a string should not fail");

        for activation_id in &run.activation_ids {
            let activation = run
                .activations
                .get(activation_id)
                .expect("activation ids should match activation map");
            let missing = if activation.missing_stop_contract.is_empty() {
                "none".to_string()
            } else {
                activation.missing_stop_contract.join(", ")
            };
            write!(
                body,
                "<tr><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
                escape_html(&activation.activation_id),
                escape_html(&activation.node_id),
                escape_html(&missing)
            )
            .expect("writing to a string should not fail");
        }

        body.push_str("</tbody></table></section>\n");
    }

    body
}

fn missing_stop_contract(state: &RuntimeState, activation: &runtime::Activation) -> Vec<String> {
    let mut missing = Vec::new();
    for artifact_key in activation.stop_contract.required_artifacts() {
        if !activation.context.contains_key(artifact_key) {
            missing.push(format!("artifact:{artifact_key}"));
        }
    }
    for effect_key in activation.stop_contract.required_effects() {
        if !state.effects.contains_key(&(
            activation.run_id.clone(),
            activation.activation_id.clone(),
            effect_key.clone(),
        )) {
            missing.push(format!("effect:{effect_key}"));
        }
    }
    missing
}

fn flow_lock_mode_name(mode: runtime::FlowLockMode) -> &'static str {
    match mode {
        runtime::FlowLockMode::FutureActivations => "future_activations",
        runtime::FlowLockMode::CheckpointRestart => "checkpoint_restart",
    }
}

fn activation_status_name(status: runtime::ActivationStatus) -> &'static str {
    match status {
        runtime::ActivationStatus::Pending => "pending",
        runtime::ActivationStatus::Starting => "starting",
        runtime::ActivationStatus::Running => "running",
        runtime::ActivationStatus::WaitingForStop => "waiting_for_stop",
        runtime::ActivationStatus::ValidatingStop => "validating_stop",
        runtime::ActivationStatus::Blocked => "blocked",
        runtime::ActivationStatus::Completed => "completed",
        runtime::ActivationStatus::Failed => "failed",
        runtime::ActivationStatus::Cancelled => "cancelled",
    }
}

fn run_status_name(status: runtime::RunStatus) -> &'static str {
    match status {
        runtime::RunStatus::PendingReview => "pending_review",
        runtime::RunStatus::Ready => "ready",
        runtime::RunStatus::Running => "running",
        runtime::RunStatus::Paused => "paused",
        runtime::RunStatus::Blocked => "blocked",
        runtime::RunStatus::Quiescent => "quiescent",
        runtime::RunStatus::Completed => "completed",
        runtime::RunStatus::Failed => "failed",
        runtime::RunStatus::Stopping => "stopping",
        runtime::RunStatus::Stopped => "stopped",
    }
}

pub(crate) fn escape_script_json(json: &str) -> String {
    json.replace("</", "<\\/")
}

pub(crate) fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn serve_browser_stream(mut stream: TcpStream, html: &str, snapshot_json: &str) -> io::Result<()> {
    let mut request_bytes = Vec::new();
    let mut buffer = [0; 512];
    loop {
        let bytes = stream.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        request_bytes.extend_from_slice(&buffer[..bytes]);
        if http_headers_complete(&request_bytes) || request_bytes.len() >= 8192 {
            break;
        }
    }
    let request = String::from_utf8_lossy(&request_bytes);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    match path {
        "/" => write_http_response(&mut stream, "200 OK", "text/html; charset=utf-8", html),
        "/snapshot.json" => {
            write_http_response(&mut stream, "200 OK", "application/json", snapshot_json)
        }
        _ => write_http_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            "not found\n",
        ),
    }
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn http_headers_complete(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"\r\n\r\n")
        || bytes.windows(2).any(|window| window == b"\n\n")
}

fn io_other(err: serde_json::Error) -> io::Error {
    io::Error::other(err)
}
