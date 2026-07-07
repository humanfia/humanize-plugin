use crate::flow;
use serde_json::{Map, Value, json};

pub(crate) fn flow_draft_json(draft: &flow::FlowDraft) -> Value {
    json!({
        "nodes": draft.nodes.iter().map(flow_node_json).collect::<Vec<_>>(),
        "contracts": draft.contracts.iter().map(flow_contract_json).collect::<Vec<_>>(),
        "routes": draft.routes.iter().map(flow_route_json).collect::<Vec<_>>(),
        "resources": draft.resources.iter().map(flow_resource_json).collect::<Vec<_>>(),
        "imports": draft.imports.iter().map(flow_import_json).collect::<Vec<_>>(),
        "policies": flow_policies_json(&draft.policies),
        "extensions": draft.extensions,
    })
}

fn flow_node_json(node: &flow::FlowNode) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(node.id));
    insert_optional_string(&mut object, "contract_id", node.contract_id.as_deref());
    if let Some(action) = &node.action {
        object.insert("action".into(), node_action_json(action));
    }
    object.insert(
        "write_scopes".into(),
        Value::Array(write_scopes_json(&node.write_scopes)),
    );
    object.insert("extensions".into(), json!(node.extensions));
    Value::Object(object)
}

fn node_action_json(action: &flow::NodeAction) -> Value {
    let mut object = Map::new();
    object.insert("driver".into(), json!(node_driver_name(action.driver)));
    insert_optional_string(&mut object, "prompt_ref", action.prompt_ref.as_deref());
    object.insert("resource_refs".into(), json!(action.resource_refs));
    object.insert("reads".into(), json!(action.reads));
    object.insert("writes".into(), json!(action.writes));
    insert_optional_string(
        &mut object,
        "verdict_artifact",
        action.verdict_artifact.as_deref(),
    );
    Value::Object(object)
}

fn flow_contract_json(contract: &flow::FlowContract) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(contract.id));
    insert_optional_string(
        &mut object,
        "completion",
        contract.completion.as_ref().map(contract_completion_name),
    );
    object.insert(
        "artifacts".into(),
        Value::Array(contract.artifacts.iter().map(flow_artifact_json).collect()),
    );
    Value::Object(object)
}

fn flow_artifact_json(artifact: &flow::ContractArtifact) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(artifact.id));
    insert_optional_string(
        &mut object,
        "schema_resource_id",
        artifact.schema_resource_id.as_deref(),
    );
    Value::Object(object)
}

fn flow_route_json(route: &flow::FlowRoute) -> Value {
    let mut object = Map::new();
    object.insert("predicate".into(), json!(route.predicate));
    insert_optional_string(&mut object, "for_each", route.for_each.as_deref());
    object.insert("activate".into(), json!(route.activate));
    Value::Object(object)
}

fn flow_resource_json(resource: &flow::FlowResource) -> Value {
    json!({
        "id": resource.id,
        "kind": resource_kind_name(&resource.kind),
        "source": resource.source,
    })
}

fn flow_import_json(import: &flow::FlowImport) -> Value {
    let mut object = Map::new();
    object.insert("resource_id".into(), json!(import.resource_id));
    insert_optional_string(&mut object, "alias", import.alias.as_deref());
    Value::Object(object)
}

fn insert_optional_string(object: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        object.insert(key.into(), Value::String(value.into()));
    }
}

fn flow_policies_json(policies: &flow::FlowPolicies) -> Value {
    json!({
        "write_scopes": write_scopes_json(&policies.write_scopes),
    })
}

fn write_scopes_json(scopes: &[flow::WriteScope]) -> Vec<Value> {
    scopes.iter().map(write_scope_json).collect()
}

fn write_scope_json(scope: &flow::WriteScope) -> Value {
    match scope {
        flow::WriteScope::Artifact(value) => json!(format!("artifact:{value}")),
        flow::WriteScope::Resource(value) => json!(format!("resource:{value}")),
        flow::WriteScope::Workspace => json!("workspace"),
        flow::WriteScope::System => json!("system"),
    }
}

fn contract_completion_name(completion: &flow::ContractCompletion) -> &'static str {
    match completion {
        flow::ContractCompletion::Manual => "manual",
        flow::ContractCompletion::AllArtifacts => "all_artifacts",
    }
}

fn node_driver_name(driver: flow::NodeDriver) -> &'static str {
    match driver {
        flow::NodeDriver::Agent => "agent",
        flow::NodeDriver::Script => "script",
        flow::NodeDriver::Review => "review",
        flow::NodeDriver::Human => "human",
    }
}

fn resource_kind_name(kind: &flow::ResourceKind) -> &'static str {
    match kind {
        flow::ResourceKind::Schema => "schema",
        flow::ResourceKind::Rule => "rule",
        flow::ResourceKind::Profile => "profile",
        flow::ResourceKind::View => "view",
        flow::ResourceKind::Prompt => "prompt",
        flow::ResourceKind::Script => "script",
        flow::ResourceKind::Flow => "flow",
        flow::ResourceKind::Readme => "readme",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_draft_json_omits_absent_optional_fields_and_keeps_present_snake_case_fields() {
        let draft = flow::FlowDraft {
            nodes: vec![
                flow::FlowNode {
                    id: "root".into(),
                    ..flow::FlowNode::default()
                },
                flow::FlowNode {
                    id: "review".into(),
                    contract_id: Some("contract.review".into()),
                    action: Some(flow::NodeAction {
                        driver: flow::NodeDriver::Human,
                        prompt_ref: Some("prompt.review".into()),
                        resource_refs: vec!["script.collect".into()],
                        reads: vec!["artifact.brief".into()],
                        writes: vec!["artifact.report".into()],
                        verdict_artifact: Some("artifact.review_verdict".into()),
                    }),
                    ..flow::FlowNode::default()
                },
                flow::FlowNode {
                    id: "script".into(),
                    action: Some(flow::NodeAction {
                        driver: flow::NodeDriver::Script,
                        prompt_ref: None,
                        resource_refs: Vec::new(),
                        reads: Vec::new(),
                        writes: Vec::new(),
                        verdict_artifact: None,
                    }),
                    ..flow::FlowNode::default()
                },
            ],
            contracts: vec![
                flow::FlowContract {
                    id: "contract.root".into(),
                    completion: None,
                    artifacts: vec![flow::ContractArtifact {
                        id: "brief".into(),
                        schema_resource_id: None,
                    }],
                },
                flow::FlowContract {
                    id: "contract.review".into(),
                    completion: Some(flow::ContractCompletion::AllArtifacts),
                    artifacts: vec![flow::ContractArtifact {
                        id: "report".into(),
                        schema_resource_id: Some("schema.report".into()),
                    }],
                },
            ],
            routes: vec![
                flow::FlowRoute {
                    predicate: "exists(artifact.brief)".into(),
                    for_each: None,
                    activate: "review".into(),
                },
                flow::FlowRoute {
                    predicate: "exists(resource.ticket)".into(),
                    for_each: Some("resource.ticket".into()),
                    activate: "root".into(),
                },
            ],
            resources: Vec::new(),
            imports: vec![
                flow::FlowImport {
                    resource_id: "prompt.audit".into(),
                    alias: None,
                },
                flow::FlowImport {
                    resource_id: "prompt.fix".into(),
                    alias: Some("fix_prompt".into()),
                },
            ],
            policies: flow::FlowPolicies::default(),
            extensions: Vec::new(),
        };

        let value = flow_draft_json(&draft);

        assert!(value["nodes"][0].get("action").is_none());
        assert!(value["nodes"][0].get("contract_id").is_none());
        assert_eq!(value["nodes"][1]["contract_id"], "contract.review");
        assert_eq!(value["nodes"][1]["action"]["driver"], "human");
        assert_eq!(value["nodes"][1]["action"]["prompt_ref"], "prompt.review");
        assert_eq!(
            value["nodes"][1]["action"]["resource_refs"],
            json!(["script.collect"])
        );
        assert_eq!(
            value["nodes"][1]["action"]["reads"],
            json!(["artifact.brief"])
        );
        assert_eq!(
            value["nodes"][1]["action"]["writes"],
            json!(["artifact.report"])
        );
        assert_eq!(
            value["nodes"][1]["action"]["verdict_artifact"],
            "artifact.review_verdict"
        );
        assert!(
            value["nodes"][2]["action"]
                .get("verdict_artifact")
                .is_none()
        );
        assert!(value["nodes"][2]["action"].get("prompt_ref").is_none());
        assert_eq!(value["nodes"][2]["action"]["resource_refs"], json!([]),);
        assert!(value["contracts"][0].get("completion").is_none());
        assert!(
            value["contracts"][0]["artifacts"][0]
                .get("schema_resource_id")
                .is_none()
        );
        assert_eq!(
            value["contracts"][1]["artifacts"][0]["schema_resource_id"],
            "schema.report"
        );
        assert!(value["routes"][0].get("for_each").is_none());
        assert_eq!(value["routes"][1]["for_each"], "resource.ticket");
        assert!(value["imports"][0].get("alias").is_none());
        assert_eq!(value["imports"][1]["alias"], "fix_prompt");
    }
}
