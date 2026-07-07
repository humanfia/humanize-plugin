use crate::adapters::tmux::CommandRunner;
use crate::flow;
use crate::view::{
    AdapterCapabilityReview, DiffEntry, FlowGraph, FlowGraphEdge, FlowGraphNode,
    FlowReviewContract, FlowReviewNode, FlowReviewRoute, FlowReviewSnapshot, FlowValueFlow,
    FlowVisualDiff, ReviewRisk, render_flow_review_document,
};
use serde_json::{Value, json};

use super::{
    FlowReviewRecord, FlowReviewStatus, McpServer, ToolCallResult, ToolError,
    diagnostic_severity_name, optional_string, require_string, stable_hash,
};

impl<R: CommandRunner> McpServer<R> {
    pub(super) fn prepare_flow_review(
        &mut self,
        arguments: &Value,
    ) -> Result<ToolCallResult, ToolError> {
        let (lock_id, content_hash) = self.require_flow_lock_binding_from_arguments(arguments)?;
        let lock = self
            .state
            .flow_locks
            .get(&lock_id)
            .ok_or_else(|| ToolError::invalid("flow lock not found"))?;
        let title = optional_string(arguments, &["title"])?.unwrap_or("Flow review");
        let review_id = review_id_for(&lock_id, &content_hash);
        let status = self
            .state
            .reviews
            .get(&review_id)
            .map(|record| record.status)
            .unwrap_or(FlowReviewStatus::Pending);
        let snapshot = build_flow_review_snapshot(title, status, lock);
        let document = render_flow_review_document(&snapshot)
            .map_err(|_| ToolError::invalid("review render failed"))?;
        let snapshot_json = serde_json::to_value(&snapshot)
            .map_err(|_| ToolError::invalid("review serialization failed"))?;

        self.state
            .flow_review_index
            .insert(lock_id.clone(), review_id.clone());
        self.state
            .reviews
            .entry(review_id.clone())
            .or_insert(FlowReviewRecord {
                review_id: review_id.clone(),
                lock_id: lock_id.clone(),
                content_hash: content_hash.clone(),
                status: FlowReviewStatus::Pending,
                reason: None,
            });

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "review_id": review_id,
            "flow_lock_id": lock_id,
            "lock_id": lock_id,
            "content_hash": content_hash,
            "review_status": status.as_str(),
            "document": document,
            "snapshot": snapshot_json
        })))
    }

    pub(super) fn approve_flow_review(
        &mut self,
        arguments: &Value,
    ) -> Result<ToolCallResult, ToolError> {
        let review_id = require_string(arguments, &["review_id", "reviewId"])?;
        let decision = require_string(arguments, &["decision", "status", "action"])?;
        let status = match decision {
            "approved" | "approve" => FlowReviewStatus::Approved,
            "bypassed" | "bypass" => FlowReviewStatus::Bypassed,
            "rejected" | "reject" => FlowReviewStatus::Rejected,
            value => {
                return Err(ToolError::invalid(format!(
                    "unknown review decision: {value}"
                )));
            }
        };
        let reason = optional_string(arguments, &["reason"])?.map(str::to_string);
        if status == FlowReviewStatus::Bypassed
            && reason
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            return Err(ToolError::invalid(
                "reason is required when bypassing review",
            ));
        }
        let Some(record) = self.state.reviews.get_mut(review_id) else {
            return Ok(ToolCallResult::error(json!({
                "ok": false,
                "review_id": review_id,
                "error": "flow review not found"
            })));
        };
        record.status = status;
        record.reason = reason.clone();

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "review_id": review_id,
            "flow_lock_id": record.lock_id,
            "lock_id": record.lock_id,
            "content_hash": record.content_hash,
            "review_status": status.as_str(),
            "reason": reason
        })))
    }
}

fn review_id_for(lock_id: &str, content_hash: &str) -> String {
    format!(
        "review_{:016x}",
        stable_hash(&format!("{lock_id}:{content_hash}"))
    )
}

fn build_flow_review_snapshot(
    title: &str,
    status: FlowReviewStatus,
    lock: &flow::FlowLock,
) -> FlowReviewSnapshot {
    let draft = lock.draft();
    let first_node = draft
        .nodes
        .first()
        .map(|node| node.id.as_str())
        .unwrap_or("root");
    let diagnostics = lock.diagnostics();
    let graph_nodes = draft
        .nodes
        .iter()
        .map(|node| FlowGraphNode {
            id: node.id.clone(),
            label: review_label(&node.id),
            kind: node
                .action
                .as_ref()
                .map(|action| node_driver_name(action.driver).to_string())
                .unwrap_or_else(|| "node".to_string()),
        })
        .collect::<Vec<_>>();
    let graph_edges = draft
        .routes
        .iter()
        .map(|route| FlowGraphEdge {
            from: first_node.to_string(),
            to: route.activate.clone(),
            label: route.predicate.clone(),
        })
        .collect::<Vec<_>>();
    let nodes = draft
        .nodes
        .iter()
        .map(|node| FlowReviewNode {
            id: node.id.clone(),
            label: review_label(&node.id),
            contract_id: node
                .contract_id
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            status: "present".to_string(),
            summary: node_review_summary(node),
        })
        .collect::<Vec<_>>();
    let routes = draft
        .routes
        .iter()
        .enumerate()
        .map(|(index, route)| FlowReviewRoute {
            id: format!("route-{index}"),
            from: first_node.to_string(),
            to: route.activate.clone(),
            predicate: route.predicate.clone(),
            outcome: format!("activates {}", route.activate),
        })
        .collect::<Vec<_>>();
    let contracts = flow::NodeContract::from_draft(draft)
        .into_iter()
        .map(|contract| FlowReviewContract {
            id: contract
                .contract_id
                .clone()
                .unwrap_or_else(|| format!("contract.{}", contract.node_id)),
            node_id: contract.node_id.clone(),
            required_artifacts: contract
                .artifact_requirements
                .iter()
                .filter(|artifact| artifact.required)
                .map(|artifact| artifact.id.clone())
                .collect(),
            required_effects: contract
                .effect_requirements
                .iter()
                .filter(|effect| effect.required)
                .map(|effect| effect.id.clone())
                .collect(),
            summary: contract_review_summary(&contract),
        })
        .collect::<Vec<_>>();
    let capabilities = flow::AdapterCapability::from_draft(draft)
        .into_iter()
        .flat_map(adapter_capability_reviews)
        .collect::<Vec<_>>();
    let artifact_flows = draft
        .routes
        .iter()
        .filter_map(|route| {
            artifact_fact_from_predicate(&route.predicate).map(|artifact| FlowValueFlow {
                source: first_node.to_string(),
                target: route.activate.clone(),
                value: artifact,
            })
        })
        .collect::<Vec<_>>();
    let risks = if diagnostics.is_empty() {
        vec![ReviewRisk {
            id: "risk.review".to_string(),
            severity: "low".to_string(),
            summary: "No checker diagnostics were attached to this lock.".to_string(),
            mitigation: "Review graph, contracts, routes, capabilities, and diff before approval."
                .to_string(),
        }]
    } else {
        diagnostics
            .iter()
            .map(|diagnostic| ReviewRisk {
                id: format!("risk.{}", diagnostic.code.to_ascii_lowercase()),
                severity: diagnostic_severity_name(diagnostic.severity_level).to_string(),
                summary: diagnostic.message.clone(),
                mitigation: diagnostic
                    .fix_hint
                    .clone()
                    .unwrap_or_else(|| "Review the diagnostic before approval.".to_string()),
            })
            .collect()
    };

    FlowReviewSnapshot {
        title: title.to_string(),
        review_status: status.as_str().to_string(),
        graph: FlowGraph {
            nodes: graph_nodes,
            edges: graph_edges,
        },
        nodes,
        routes,
        contracts,
        capabilities,
        artifact_flows,
        effect_flows: Vec::new(),
        runtime_budgets: Vec::new(),
        risks,
        dynamic_diff: review_diff(draft),
    }
}

fn review_label(id: &str) -> String {
    id.replace(['_', '-'], " ")
}

fn node_review_summary(node: &flow::FlowNode) -> String {
    match &node.action {
        Some(action) => format!("Runs with {} driver.", node_driver_name(action.driver)),
        None => "No adapter action declared.".to_string(),
    }
}

fn contract_review_summary(contract: &flow::NodeContract) -> String {
    let artifacts = contract.artifact_requirements.len();
    let effects = contract.effect_requirements.len();
    format!("Requires {artifacts} artifact entries and {effects} effect entries.")
}

fn adapter_capability_reviews(capability: flow::AdapterCapability) -> Vec<AdapterCapabilityReview> {
    let adapter = node_driver_name(capability.driver).to_string();
    let mut reviews = Vec::new();
    for required in capability.requires {
        reviews.push(AdapterCapabilityReview {
            adapter: adapter.clone(),
            capability: required,
            present: true,
            detail: format!("required by {}", capability.node_id),
        });
    }
    for preferred in capability.prefers {
        reviews.push(AdapterCapabilityReview {
            adapter: adapter.clone(),
            capability: preferred,
            present: true,
            detail: format!("preferred by {}", capability.node_id),
        });
    }
    for accepted in capability.accepts {
        reviews.push(AdapterCapabilityReview {
            adapter: adapter.clone(),
            capability: accepted,
            present: true,
            detail: format!("accepted by {}", capability.node_id),
        });
    }
    reviews
}

fn node_driver_name(driver: flow::NodeDriver) -> &'static str {
    match driver {
        flow::NodeDriver::Agent => "agent",
        flow::NodeDriver::Script => "script",
        flow::NodeDriver::Review => "review",
        flow::NodeDriver::Human => "human",
    }
}

fn artifact_fact_from_predicate(predicate: &str) -> Option<String> {
    let trimmed = predicate.trim();
    if let Some(inner) = trimmed
        .strip_prefix("exists(")
        .and_then(|value| value.strip_suffix(')'))
    {
        if inner.starts_with("artifact.") {
            return Some(inner.to_string());
        }
    }
    trimmed
        .starts_with("artifact.")
        .then(|| trimmed.to_string())
}

fn review_diff(draft: &flow::FlowDraft) -> FlowVisualDiff {
    FlowVisualDiff {
        added_nodes: draft
            .nodes
            .iter()
            .map(|node| DiffEntry::new(node.id.clone(), "node is present in proposed flow"))
            .collect(),
        removed_nodes: Vec::new(),
        changed_nodes: Vec::new(),
        added_routes: draft
            .routes
            .iter()
            .enumerate()
            .map(|(index, route)| {
                DiffEntry::new(
                    format!("route-{index}"),
                    format!("activates {}", route.activate),
                )
            })
            .collect(),
        removed_routes: Vec::new(),
        changed_routes: Vec::new(),
        added_contracts: draft
            .contracts
            .iter()
            .map(|contract| DiffEntry::new(contract.id.clone(), "contract is present"))
            .collect(),
        removed_contracts: Vec::new(),
        changed_contracts: Vec::new(),
        capability_changes: flow::AdapterCapability::from_draft(draft)
            .into_iter()
            .map(|capability| {
                DiffEntry::new(
                    capability.node_id,
                    format!("uses {} driver", node_driver_name(capability.driver)),
                )
            })
            .collect(),
        risk_changes: Vec::new(),
    }
}
