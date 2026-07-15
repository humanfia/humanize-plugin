use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Serialize;

use crate::flow;

use super::{RuntimeBudgetSnapshot, escape_html, escape_script_json};

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct FlowReviewSnapshot {
    pub title: String,
    pub review_status: String,
    pub graph: FlowGraph,
    pub nodes: Vec<FlowReviewNode>,
    pub routes: Vec<FlowReviewRoute>,
    pub contracts: Vec<FlowReviewContract>,
    pub capabilities: Vec<AdapterCapabilityReview>,
    pub artifact_flows: Vec<FlowValueFlow>,
    pub effect_flows: Vec<FlowValueFlow>,
    pub runtime_budgets: Vec<RuntimeBudgetSnapshot>,
    pub risks: Vec<ReviewRisk>,
    pub dynamic_diff: FlowVisualDiff,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct FlowGraph {
    pub nodes: Vec<FlowGraphNode>,
    pub edges: Vec<FlowGraphEdge>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct FlowGraphNode {
    pub id: String,
    pub label: String,
    pub kind: String,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct FlowGraphEdge {
    pub from: String,
    pub to: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct FlowReviewNode {
    pub id: String,
    pub label: String,
    pub contract_id: String,
    pub status: String,
    pub summary: String,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct FlowReviewRoute {
    pub id: String,
    pub from: String,
    pub to: String,
    pub predicate: String,
    pub for_each: Option<String>,
    pub outcome: String,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct FlowReviewContract {
    pub id: String,
    pub node_id: String,
    pub required_artifacts: Vec<String>,
    pub required_effects: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct AdapterCapabilityReview {
    pub adapter: String,
    pub capability: String,
    pub present: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct FlowValueFlow {
    pub source: String,
    pub target: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct ReviewRisk {
    pub id: String,
    pub severity: String,
    pub summary: String,
    pub mitigation: String,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct FlowVisualDiff {
    pub added_nodes: Vec<DiffEntry>,
    pub removed_nodes: Vec<DiffEntry>,
    pub changed_nodes: Vec<DiffEntry>,
    pub added_routes: Vec<DiffEntry>,
    pub removed_routes: Vec<DiffEntry>,
    pub changed_routes: Vec<DiffEntry>,
    pub added_contracts: Vec<DiffEntry>,
    pub removed_contracts: Vec<DiffEntry>,
    pub changed_contracts: Vec<DiffEntry>,
    pub capability_changes: Vec<DiffEntry>,
    pub risk_changes: Vec<DiffEntry>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct DiffEntry {
    pub id: String,
    pub detail: String,
}

impl DiffEntry {
    pub fn new(id: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            detail: detail.into(),
        }
    }
}

pub fn derive_flow_graph(draft: &flow::FlowDraft) -> FlowGraph {
    let producers = artifact_producers(draft);
    let mut facts = BTreeSet::new();
    for route in &draft.routes {
        facts.insert(route.predicate.fact_ref().clone());
        if let Some(for_each) = &route.for_each {
            facts.insert(for_each.fact_ref());
        }
    }

    let mut nodes = draft
        .nodes
        .iter()
        .map(|node| FlowGraphNode {
            id: node.id.clone(),
            label: node.id.replace(['_', '-'], " "),
            kind: "work".to_string(),
        })
        .collect::<Vec<_>>();
    nodes.extend(facts.iter().map(|fact_ref| FlowGraphNode {
        id: fact_node_id(fact_ref),
        label: fact_ref.to_string(),
        kind: "fact".to_string(),
    }));
    nodes.sort_by(|left, right| left.id.cmp(&right.id));

    let mut edges = Vec::new();
    for fact_ref in &facts {
        let flow::FactRef::Artifact { .. } = fact_ref else {
            continue;
        };
        if let Some(producers) = producers.get(fact_ref)
            && producers.len() == 1
        {
            edges.push(FlowGraphEdge {
                from: producers.iter().next().expect("one producer").clone(),
                to: fact_node_id(fact_ref),
                label: "produces".to_string(),
            });
        }
    }
    for route in &draft.routes {
        let predicate_fact = route.predicate.fact_ref();
        let for_each_fact = route.for_each.as_ref().map(flow::ArtifactRef::fact_ref);
        let label = if for_each_fact.as_ref() == Some(predicate_fact) {
            format!(
                "{} | for each {}",
                route.predicate,
                route.for_each.as_ref().expect("matching fanout fact")
            )
        } else {
            route.predicate.to_string()
        };
        edges.push(FlowGraphEdge {
            from: fact_node_id(predicate_fact),
            to: route.activate.clone(),
            label,
        });
        if let Some(for_each_fact) = for_each_fact
            && &for_each_fact != predicate_fact
        {
            edges.push(FlowGraphEdge {
                from: fact_node_id(&for_each_fact),
                to: route.activate.clone(),
                label: format!("for each {}", route.for_each.as_ref().expect("fanout fact")),
            });
        }
    }
    edges.sort_by(|left, right| {
        left.from
            .cmp(&right.from)
            .then(left.to.cmp(&right.to))
            .then(left.label.cmp(&right.label))
    });

    FlowGraph { nodes, edges }
}

fn artifact_producers(draft: &flow::FlowDraft) -> BTreeMap<flow::FactRef, BTreeSet<String>> {
    let contracts = draft
        .contracts
        .iter()
        .map(|contract| (contract.id.as_str(), contract))
        .collect::<BTreeMap<_, _>>();
    let mut producers = BTreeMap::<flow::FactRef, BTreeSet<String>>::new();
    for node in &draft.nodes {
        if let Some(action) = &node.action {
            for fact_path in action.writes.iter().chain(action.verdict_artifact.iter()) {
                if let Some(fact_ref) = artifact_fact_from_action_ref(fact_path) {
                    producers
                        .entry(fact_ref)
                        .or_default()
                        .insert(node.id.clone());
                }
            }
        }
        if let Some(contract) = node
            .contract_id
            .as_deref()
            .and_then(|contract_id| contracts.get(contract_id).copied())
        {
            for artifact in &contract.artifacts {
                if let Ok(fact_ref) = flow::FactRef::artifact(&artifact.id) {
                    producers
                        .entry(fact_ref)
                        .or_default()
                        .insert(node.id.clone());
                }
            }
        }
    }
    producers
}

fn artifact_fact_from_action_ref(value: &str) -> Option<flow::FactRef> {
    flow::FactRef::artifact(value.strip_prefix("artifact.")?).ok()
}

fn fact_node_id(fact_ref: &flow::FactRef) -> String {
    format!("fact:{fact_ref}")
}

pub fn render_flow_review_document(snapshot: &FlowReviewSnapshot) -> serde_json::Result<String> {
    let snapshot_json = escape_non_ascii(&escape_script_json(&serde_json::to_string(snapshot)?));
    let mut body = String::new();

    write!(
        body,
        "<header><p class=\"eyebrow\">Flow Review</p><h1>{}</h1><p class=\"status\">Review status: <strong>{}</strong></p></header>",
        text(&snapshot.title),
        text(&snapshot.review_status)
    )
    .expect("writing to a string should not fail");

    render_graph(&mut body, &snapshot.graph);
    render_nodes(&mut body, &snapshot.nodes);
    render_routes(&mut body, &snapshot.routes);
    render_contracts(&mut body, &snapshot.contracts);
    render_capabilities(&mut body, &snapshot.capabilities);
    render_value_flows(&mut body, "Artifact Flow", &snapshot.artifact_flows);
    render_value_flows(&mut body, "Effect Flow", &snapshot.effect_flows);
    render_budgets(&mut body, &snapshot.runtime_budgets);
    render_risks(&mut body, &snapshot.risks);
    render_diff(&mut body, &snapshot.dynamic_diff);

    let html = format!(
        concat!(
            "<!doctype html>\n",
            "<html lang=\"en\">\n",
            "<head>\n",
            "<meta charset=\"utf-8\">\n",
            "<title>Flow Review</title>\n",
            "<style>",
            ":root{{--ink:#162128;--muted:#5c6670;--line:#cfd7dd;--paper:#fbfaf4;--panel:#ffffff;--blue:#155a73;--green:#386d43;--gold:#8a5a14;--red:#9b3c35;}}",
            "body{{font-family:system-ui,-apple-system,Segoe UI,sans-serif;margin:0;color:var(--ink);background:var(--paper);}}",
            "main{{max-width:1120px;margin:0 auto;padding:28px;}}",
            "header{{border-bottom:4px solid var(--blue);padding-bottom:16px;margin-bottom:20px;}}",
            ".eyebrow{{color:var(--gold);font-weight:700;text-transform:uppercase;letter-spacing:0;margin:0 0 6px;}}",
            "h1{{font-size:32px;margin:0 0 8px;letter-spacing:0;}}",
            "h2{{font-size:20px;margin:0 0 12px;letter-spacing:0;}}",
            "section{{background:var(--panel);border:1px solid var(--line);border-left:5px solid var(--green);border-radius:8px;padding:16px;margin:16px 0;}}",
            ".status{{margin:0;color:var(--muted);}}",
            ".graph-grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:10px;margin-bottom:12px;}}",
            ".node{{border:1px solid var(--line);border-top:4px solid var(--blue);border-radius:8px;padding:10px;background:#f6fbfd;}}",
            ".node strong,.node span,.node small{{display:block;overflow-wrap:anywhere;}}",
            ".node small{{color:var(--muted);margin-top:4px;}}",
            "table{{border-collapse:collapse;width:100%;font-size:14px;}}",
            "th,td{{border-top:1px solid #e3e7e9;padding:8px;text-align:left;vertical-align:top;overflow-wrap:anywhere;}}",
            "th{{color:#26333c;background:#eef4f2;}}",
            "code{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;}}",
            ".pill{{display:inline-block;border-radius:999px;padding:2px 8px;font-size:12px;font-weight:700;}}",
            ".present{{background:#e6f4e9;color:var(--green);}}",
            ".missing{{background:#f8e7e5;color:var(--red);}}",
            ".diff-group{{margin:10px 0;}}",
            ".diff-group h3{{font-size:15px;margin:0 0 6px;color:var(--blue);}}",
            "ul{{margin:0;padding-left:20px;}}",
            "li{{margin:4px 0;}}",
            "</style>\n",
            "</head>\n",
            "<body><main>",
            "{}",
            "</main><script type=\"application/json\" id=\"flow-review-snapshot\">{}</script></body>\n",
            "</html>\n"
        ),
        body, snapshot_json
    );

    Ok(escape_non_ascii(&html))
}

fn render_graph(body: &mut String, graph: &FlowGraph) {
    body.push_str("<section id=\"flow-review-graph\"><h2>Workflow Graph</h2>");
    body.push_str("<div class=\"graph-grid\">");
    for node in &graph.nodes {
        write!(
            body,
            "<div class=\"node\"><strong>{}</strong><span>{}</span><small>{}</small></div>",
            text(&node.id),
            text(&node.label),
            text(&node.kind)
        )
        .expect("writing to a string should not fail");
    }
    body.push_str("</div>");
    body.push_str("<table><thead><tr><th>Edge</th><th>Label</th></tr></thead><tbody>");
    for edge in &graph.edges {
        write!(
            body,
            "<tr><td><code>{} -> {}</code></td><td>{}</td></tr>",
            text(&edge.from),
            text(&edge.to),
            text(&edge.label)
        )
        .expect("writing to a string should not fail");
    }
    body.push_str("</tbody></table></section>");
}

fn render_nodes(body: &mut String, nodes: &[FlowReviewNode]) {
    body.push_str("<section><h2>Node Contract Summary</h2>");
    body.push_str("<table><thead><tr><th>Node</th><th>Contract</th><th>Status</th><th>Summary</th></tr></thead><tbody>");
    for node in nodes {
        write!(
            body,
            "<tr><td><code>{}</code><br>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
            text(&node.id),
            text(&node.label),
            text(&node.contract_id),
            text(&node.status),
            text(&node.summary)
        )
        .expect("writing to a string should not fail");
    }
    body.push_str("</tbody></table></section>");
}

fn render_routes(body: &mut String, routes: &[FlowReviewRoute]) {
    body.push_str("<section><h2>Route Predicates</h2>");
    body.push_str("<table><thead><tr><th>Route</th><th>Path</th><th>Predicate</th><th>Fanout</th><th>Outcome</th></tr></thead><tbody>");
    for route in routes {
        write!(
            body,
            "<tr><td><code>{}</code></td><td><code>{} -> {}</code></td><td><code>{}</code></td><td><code>{}</code></td><td>{}</td></tr>",
            text(&route.id),
            text(&route.from),
            text(&route.to),
            text(&route.predicate),
            text(route.for_each.as_deref().unwrap_or("none")),
            text(&route.outcome)
        )
        .expect("writing to a string should not fail");
    }
    body.push_str("</tbody></table></section>");
}

fn render_contracts(body: &mut String, contracts: &[FlowReviewContract]) {
    body.push_str("<section><h2>Contracts</h2>");
    body.push_str("<table><thead><tr><th>Contract</th><th>Node</th><th>Artifacts</th><th>Effects</th><th>Summary</th></tr></thead><tbody>");
    for contract in contracts {
        write!(
            body,
            "<tr><td><code>{}</code></td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            text(&contract.id),
            text(&contract.node_id),
            text(&join_or_none(&contract.required_artifacts)),
            text(&join_or_none(&contract.required_effects)),
            text(&contract.summary)
        )
        .expect("writing to a string should not fail");
    }
    body.push_str("</tbody></table></section>");
}

fn render_capabilities(body: &mut String, capabilities: &[AdapterCapabilityReview]) {
    body.push_str("<section><h2>Adapter Capabilities</h2>");
    body.push_str("<table><thead><tr><th>Adapter</th><th>Capability</th><th>State</th><th>Detail</th></tr></thead><tbody>");
    for capability in capabilities {
        let state = if capability.present {
            ("present", "present")
        } else {
            ("missing", "missing")
        };
        write!(
            body,
            "<tr><td><code>{}</code></td><td><code>{}</code></td><td><span class=\"pill {}\">{}</span></td><td>{}</td></tr>",
            text(&capability.adapter),
            text(&capability.capability),
            state.0,
            state.1,
            text(&capability.detail)
        )
        .expect("writing to a string should not fail");
    }
    body.push_str("</tbody></table></section>");
}

fn render_value_flows(body: &mut String, title: &str, flows: &[FlowValueFlow]) {
    write!(body, "<section><h2>{}</h2>", text(title)).expect("writing to a string should not fail");
    body.push_str("<table><thead><tr><th>Value</th><th>Path</th></tr></thead><tbody>");
    for flow in flows {
        write!(
            body,
            "<tr><td><code>{}</code></td><td><code>{} -> {}</code></td></tr>",
            text(&flow.value),
            text(&flow.source),
            text(&flow.target)
        )
        .expect("writing to a string should not fail");
    }
    body.push_str("</tbody></table></section>");
}

fn render_budgets(body: &mut String, budgets: &[RuntimeBudgetSnapshot]) {
    body.push_str("<section><h2>Runtime Budgets</h2>");
    body.push_str("<table><thead><tr><th>Name</th><th>Used</th><th>Limit</th><th>Unit</th></tr></thead><tbody>");
    for budget in budgets {
        write!(
            body,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            text(&budget.name),
            budget.used,
            budget.limit,
            text(&budget.unit)
        )
        .expect("writing to a string should not fail");
    }
    body.push_str("</tbody></table></section>");
}

fn render_risks(body: &mut String, risks: &[ReviewRisk]) {
    body.push_str("<section><h2>Risk List</h2>");
    body.push_str("<table><thead><tr><th>Risk</th><th>Severity</th><th>Summary</th><th>Mitigation</th></tr></thead><tbody>");
    for risk in risks {
        write!(
            body,
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            text(&risk.id),
            text(&risk.severity),
            text(&risk.summary),
            text(&risk.mitigation)
        )
        .expect("writing to a string should not fail");
    }
    body.push_str("</tbody></table></section>");
}

fn render_diff(body: &mut String, diff: &FlowVisualDiff) {
    body.push_str("<section><h2>Dynamic Update Diff</h2>");
    render_diff_group(body, "Added Nodes", &diff.added_nodes);
    render_diff_group(body, "Removed Nodes", &diff.removed_nodes);
    render_diff_group(body, "Changed Nodes", &diff.changed_nodes);
    render_diff_group(body, "Added Routes", &diff.added_routes);
    render_diff_group(body, "Removed Routes", &diff.removed_routes);
    render_diff_group(body, "Changed Routes", &diff.changed_routes);
    render_diff_group(body, "Added Contracts", &diff.added_contracts);
    render_diff_group(body, "Removed Contracts", &diff.removed_contracts);
    render_diff_group(body, "Changed Contracts", &diff.changed_contracts);
    render_diff_group(body, "Capability Changes", &diff.capability_changes);
    render_diff_group(body, "Risk Changes", &diff.risk_changes);
    body.push_str("</section>");
}

fn render_diff_group(body: &mut String, title: &str, entries: &[DiffEntry]) {
    write!(body, "<div class=\"diff-group\"><h3>{}</h3>", text(title))
        .expect("writing to a string should not fail");
    if entries.is_empty() {
        body.push_str("<p>none</p></div>");
        return;
    }
    body.push_str("<ul>");
    for entry in entries {
        write!(
            body,
            "<li><code>{}</code>: {}</li>",
            text(&entry.id),
            text(&entry.detail)
        )
        .expect("writing to a string should not fail");
    }
    body.push_str("</ul></div>");
}

fn join_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(", ")
    }
}

fn text(input: &str) -> String {
    escape_non_ascii(&escape_html(input))
}

fn escape_non_ascii(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for character in input.chars() {
        if character.is_ascii() {
            output.push(character);
        } else {
            write!(output, "&#{};", character as u32).expect("writing to a string should not fail");
        }
    }
    output
}
