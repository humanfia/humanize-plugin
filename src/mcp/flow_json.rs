use crate::flow;
use serde_json::{Map, Value, json};

pub(super) fn flow_draft_json(draft: &flow::FlowDraft) -> Value {
    let mut object = Map::new();
    object.insert(
        "nodes".into(),
        Value::Array(draft.nodes.iter().map(flow_node_json).collect()),
    );
    object.insert(
        "contracts".into(),
        Value::Array(
            draft
                .contracts
                .iter()
                .map(|contract| flow_contract_json(draft, contract))
                .collect(),
        ),
    );
    object.insert(
        "routes".into(),
        Value::Array(draft.routes.iter().map(flow_route_json).collect()),
    );
    object.insert(
        "resources".into(),
        Value::Array(draft.resources.iter().map(flow_resource_json).collect()),
    );
    object.insert(
        "imports".into(),
        Value::Array(draft.imports.iter().map(flow_import_json).collect()),
    );
    object.insert("policies".into(), flow_policies_json(&draft.policies));
    let qos = flow::flow_draft_qos(draft);
    if !qos.is_default() {
        object.insert("qos".into(), flow_qos_json(&qos));
    }
    let extensions = draft
        .extensions
        .iter()
        .filter(|extension| !flow::extension_is_flow_qos(extension))
        .collect::<Vec<_>>();
    object.insert("extensions".into(), json!(extensions));
    Value::Object(object)
}

fn flow_node_json(node: &flow::FlowNode) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(node.id));
    insert_optional_string(&mut object, "contract_id", node.contract_id.as_deref());
    if let Some(action) = &node.action {
        object.insert("action".into(), node_action_json(action));
    }
    let work_profile = flow::flow_node_work_profile(node);
    if !work_profile.is_default() {
        object.insert("work_profile".into(), work_profile_json(&work_profile));
    }
    object.insert(
        "write_scopes".into(),
        Value::Array(write_scopes_json(&node.write_scopes)),
    );
    let extensions = node
        .extensions
        .iter()
        .filter(|extension| !flow::extension_is_node_work_profile(extension))
        .collect::<Vec<_>>();
    object.insert("extensions".into(), json!(extensions));
    Value::Object(object)
}

fn work_profile_json(profile: &flow::WorkProfile) -> Value {
    json!({
        "intent": work_intent_name(profile.intent),
        "workspace_access": workspace_access_name(profile.workspace_access),
        "tool_execution": tool_execution_name(profile.tool_execution),
        "network_access": network_access_name(profile.network_access),
    })
}

fn flow_qos_json(qos: &flow::FlowQosIntent) -> Value {
    let mut object = Map::new();
    object.insert("urgency".into(), json!(qos_urgency_name(qos.urgency)));
    insert_optional_string(
        &mut object,
        "completion_target",
        qos.completion_target.as_deref(),
    );
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

fn flow_contract_json(draft: &flow::FlowDraft, contract: &flow::FlowContract) -> Value {
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
    let effects = flow::flow_draft_contract_effects(draft, &contract.id);
    if !effects.is_empty() {
        object.insert(
            "effects".into(),
            Value::Array(effects.iter().map(flow_effect_json).collect()),
        );
    }
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

fn flow_effect_json(effect: &flow::EffectRequirement) -> Value {
    json!({
        "id": effect.id,
        "required": effect.required,
    })
}

fn flow_route_json(route: &flow::FlowRoute) -> Value {
    let mut object = Map::new();
    object.insert(
        "predicate".into(),
        serde_json::to_value(&route.predicate).expect("flow predicate serialization cannot fail"),
    );
    if let Some(for_each) = &route.for_each {
        object.insert(
            "for_each".into(),
            serde_json::to_value(for_each).expect("artifact reference serialization cannot fail"),
        );
    }
    object.insert("activate".into(), json!(route.activate));
    Value::Object(object)
}

fn flow_resource_json(resource: &flow::FlowResource) -> Value {
    json!({
        "path": resource.id,
        "kind": resource_kind_name(&resource.kind),
        "content": resource.source,
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

fn work_intent_name(intent: flow::WorkIntent) -> &'static str {
    match intent {
        flow::WorkIntent::Produce => "produce",
        flow::WorkIntent::Evaluate => "evaluate",
        flow::WorkIntent::Explore => "explore",
        flow::WorkIntent::Synthesize => "synthesize",
        flow::WorkIntent::Coordinate => "coordinate",
    }
}

fn workspace_access_name(access: flow::WorkspaceAccess) -> &'static str {
    match access {
        flow::WorkspaceAccess::None => "none",
        flow::WorkspaceAccess::ReadOnly => "read_only",
        flow::WorkspaceAccess::ReadWrite => "read_write",
    }
}

fn tool_execution_name(execution: flow::ToolExecution) -> &'static str {
    match execution {
        flow::ToolExecution::None => "none",
        flow::ToolExecution::Allowed => "allowed",
    }
}

fn network_access_name(access: flow::NetworkAccess) -> &'static str {
    match access {
        flow::NetworkAccess::None => "none",
        flow::NetworkAccess::Restricted => "restricted",
        flow::NetworkAccess::Open => "open",
    }
}

fn qos_urgency_name(urgency: flow::QosUrgency) -> &'static str {
    match urgency {
        flow::QosUrgency::Interactive => "interactive",
        flow::QosUrgency::Standard => "standard",
        flow::QosUrgency::Background => "background",
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
        flow::ResourceKind::Skill => "skill",
    }
}

pub(super) fn input_severity_name(diagnostics: &[flow::Diagnostic]) -> &'static str {
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == flow::Severity::Fatal)
    {
        "fatal"
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == flow::Severity::Error)
    {
        "error"
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == flow::Severity::Warning)
    {
        "warning"
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == flow::Severity::Note)
    {
        "note"
    } else {
        "none"
    }
}

pub(super) fn repair_candidates_json(candidates: &[flow::FlowRepairCandidate]) -> Vec<Value> {
    candidates
        .iter()
        .map(|candidate| {
            json!({
                "repair_kind": repair_kind_name(candidate.repair_kind),
                "location": candidate.location,
                "replacement": candidate.replacement
            })
        })
        .collect()
}

pub(super) fn repair_guidance_json(diagnostics: &[flow::Diagnostic]) -> Vec<Value> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code,
                "location": diagnostic.location,
                "message": diagnostic.message,
                "fix_hint": diagnostic.fix_hint,
                "why_it_matters": diagnostic.why_it_matters,
                "repairability": repairability_name(diagnostic.repairability),
                "repair_kinds": diagnostic
                    .repair_kinds
                    .iter()
                    .map(|kind| repair_kind_name(*kind))
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

pub(super) fn diagnostics_json(diagnostics: &[flow::Diagnostic]) -> Vec<Value> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code,
                "severity": severity_name(diagnostic.severity),
                "domain": diagnostic_domain_name(diagnostic.domain),
                "repairability": repairability_name(diagnostic.repairability),
                "location": diagnostic.location,
                "message": diagnostic.message,
                "fix_hint": diagnostic.fix_hint,
                "why_it_matters": diagnostic.why_it_matters,
                "repair_kinds": diagnostic
                    .repair_kinds
                    .iter()
                    .map(|kind| repair_kind_name(*kind))
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

pub(super) fn diagnostic_codes_text(diagnostics: &[flow::Diagnostic]) -> String {
    if diagnostics.is_empty() {
        "none".to_string()
    } else {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn severity_name(severity: flow::Severity) -> &'static str {
    match severity {
        flow::Severity::Fatal => "fatal",
        flow::Severity::Error => "error",
        flow::Severity::Warning => "warning",
        flow::Severity::Note => "note",
    }
}

fn diagnostic_domain_name(domain: flow::DiagnosticDomain) -> &'static str {
    match domain {
        flow::DiagnosticDomain::Package => "package",
        flow::DiagnosticDomain::Contract => "contract",
        flow::DiagnosticDomain::Resource => "resource",
        flow::DiagnosticDomain::Route => "route",
        flow::DiagnosticDomain::Policy => "policy",
        flow::DiagnosticDomain::RuntimeCompat => "runtime_compat",
    }
}

fn repairability_name(repairability: flow::Repairability) -> &'static str {
    match repairability {
        flow::Repairability::Automatic => "automatic",
        flow::Repairability::Candidate => "candidate",
        flow::Repairability::GuidanceOnly => "guidance_only",
        flow::Repairability::None => "none",
    }
}

fn repair_kind_name(kind: flow::RepairKind) -> &'static str {
    match kind {
        flow::RepairKind::AddRouteTarget => "add_route_target",
        flow::RepairKind::AddArtifactSchema => "add_artifact_schema",
        flow::RepairKind::AddContractCompletion => "add_contract_completion",
        flow::RepairKind::NarrowWriteScope => "narrow_write_scope",
        flow::RepairKind::ProvideRuntimeResource => "provide_runtime_resource",
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
                    predicate: flow::FlowPredicate::exists_artifact("brief").unwrap(),
                    for_each: None,
                    activate: "review".into(),
                },
                flow::FlowRoute {
                    predicate: flow::FlowPredicate::exists_artifact("ticket").unwrap(),
                    for_each: Some(flow::ArtifactRef::new("ticket").unwrap()),
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
        assert_eq!(value["routes"][1]["for_each"], json!({"key": "ticket"}));
        assert!(value["imports"][0].get("alias").is_none());
        assert_eq!(value["imports"][1]["alias"], "fix_prompt");
    }
}
