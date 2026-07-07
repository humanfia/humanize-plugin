use std::collections::BTreeMap;
use std::fmt::Write;

use serde::Serialize;
use serde_json::{Value, json};

use crate::runtime::{self, RuntimeState};

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
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RunSnapshot {
    pub run_id: String,
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
    pub latest_flow_lock_application: Option<String>,
    pub flow_lock_applications: BTreeMap<String, FlowLockApplicationSnapshot>,
    pub missing_stop_contracts: BTreeMap<String, Vec<String>>,
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

        Self {
            run_id: run_id.to_string(),
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
            latest_flow_lock_application: state
                .latest_flow_lock_application_by_run
                .get(run_id)
                .cloned(),
            flow_lock_applications,
            missing_stop_contracts,
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
            "latest_flow_lock_application": self.latest_flow_lock_application,
            "flow_lock_applications": self.flow_lock_applications
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ActivationSnapshot {
    pub activation_id: String,
    pub run_id: String,
    pub node_id: String,
    pub stable_key: Option<String>,
    pub context: BTreeMap<String, String>,
    pub required_artifacts: Vec<String>,
    pub required_effects: Vec<String>,
    pub flow_lock_mode: Option<String>,
    pub missing_stop_contract: Vec<String>,
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
            context: activation.context.clone(),
            required_artifacts,
            required_effects,
            flow_lock_mode: activation
                .flow_lock_mode
                .map(flow_lock_mode_name)
                .map(str::to_string),
            missing_stop_contract,
        }
    }

    fn to_context_json(&self) -> Value {
        json!({
            "activation_id": self.activation_id,
            "run_id": self.run_id,
            "node_id": self.node_id,
            "stable_key": self.stable_key,
            "context": self.context,
            "required_artifacts": self.required_artifacts,
            "required_effects": self.required_effects,
            "flow_lock_mode": self.flow_lock_mode
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
        writeln!(
            output,
            "run {} | activations {} | board v{} | messages {} | artifacts {} | effects {} | missing {}",
            run.run_id,
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
            writeln!(
                output,
                "  {} | node {} | missing {}",
                activation.activation_id, activation.node_id, missing
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

fn escape_script_json(json: &str) -> String {
    json.replace("</", "<\\/")
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
