use crate::adapters::tmux::CommandRunner;
use crate::flow;
use crate::review::{ReviewDecision, ReviewStatus};
use crate::view::{
    AdapterCapabilityReview, DiffEntry, FlowReviewContract, FlowReviewNode, FlowReviewRoute,
    FlowReviewSnapshot, FlowValueFlow, FlowVisualDiff, ReviewRisk, derive_flow_graph,
    render_flow_review_document,
};
use serde_json::{Value, json};

use super::{McpServer, ToolCallResult, ToolError, optional_string, require_string};

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
        let status = ReviewStatus::Pending;
        let snapshot = build_flow_review_snapshot(title, status, lock);
        let document = render_flow_review_document(&snapshot)
            .map_err(|_| ToolError::invalid("review render failed"))?;
        let snapshot_json = serde_json::to_value(&snapshot)
            .map_err(|_| ToolError::invalid("review serialization failed"))?;

        let record = self
            .review_store
            .prepare(lock, &snapshot_json, &document)
            .map_err(|error| ToolError::invalid(error.to_string()))?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "review_id": record.review_id(),
            "flow_lock_id": lock_id,
            "lock_id": lock_id,
            "content_hash": content_hash,
            "review_status": record.status().as_str(),
            "review_uri": record.document_uri(),
            "review_path": record.review_json_path(),
            "document_path": record.document_path(),
            "document": document,
            "snapshot": snapshot_json
        })))
    }

    pub(super) fn decide_flow_review(
        &mut self,
        arguments: &Value,
    ) -> Result<ToolCallResult, ToolError> {
        let review_id = require_string(arguments, &["review_id", "reviewId"])?;
        let decision = require_string(arguments, &["decision", "status", "action"])?;
        let status = match decision {
            "approved" | "approve" => ReviewDecision::Approved,
            "bypassed" | "bypass" => ReviewDecision::Bypassed,
            "rejected" | "reject" => ReviewDecision::Rejected,
            value => {
                return Err(ToolError::invalid(format!(
                    "unknown review decision: {value}"
                )));
            }
        };
        let reason = optional_string(arguments, &["reason"])?.map(str::to_string);
        if matches!(status, ReviewDecision::Bypassed | ReviewDecision::Rejected)
            && reason
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            return Err(ToolError::invalid(
                "reason is required when rejecting or bypassing review",
            ));
        }
        let record = self
            .review_store
            .decide(review_id, status, reason.as_deref())
            .map_err(|error| ToolError::invalid(error.to_string()))?;

        Ok(ToolCallResult::ok(json!({
            "ok": true,
            "review_id": review_id,
            "flow_lock_id": record.flow_lock_id(),
            "lock_id": record.flow_lock_id(),
            "content_hash": record.content_hash(),
            "review_status": record.status().as_str(),
            "reason": record.reason()
        })))
    }
}

fn build_flow_review_snapshot(
    title: &str,
    status: ReviewStatus,
    lock: &flow::FlowLock,
) -> FlowReviewSnapshot {
    let draft = lock.draft();
    let diagnostics = lock.diagnostics();
    let graph = derive_flow_graph(draft);
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
            from: format!("fact:{}", route.predicate.fact_ref()),
            to: route.activate.clone(),
            predicate: route.predicate.to_string(),
            for_each: route.for_each.as_ref().map(ToString::to_string),
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
        .flat_map(|route| route_artifact_facts(route).map(move |fact| (route, fact)))
        .filter_map(|(route, artifact)| {
            let fact_node = format!("fact:{artifact}");
            let source = graph
                .edges
                .iter()
                .find(|edge| edge.to == fact_node && edge.label == "produces")?
                .from
                .clone();
            Some(FlowValueFlow {
                source,
                target: route.activate.clone(),
                value: artifact.to_string(),
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
                severity: diagnostic.severity.as_str().to_string(),
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
        graph,
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

fn route_artifact_facts(route: &flow::FlowRoute) -> impl Iterator<Item = flow::FactRef> + '_ {
    let predicate = match route.predicate.fact_ref() {
        fact @ flow::FactRef::Artifact { .. } => Some(fact.clone()),
        flow::FactRef::Board { .. } => None,
    };
    predicate
        .into_iter()
        .chain(route.for_each.as_ref().map(flow::ArtifactRef::fact_ref))
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
