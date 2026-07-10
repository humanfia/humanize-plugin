use crate::flow;
use serde_json::Value;

use super::{
    ToolError, optional_array_field, optional_bool_field, optional_string_array_from_object,
    optional_string_field, require_object_arg, string_array, string_field,
};

pub(super) fn flow_draft_arg(arguments: &Value) -> Result<flow::FlowDraft, ToolError> {
    let flow = require_object_arg(arguments, &["flow"])?;
    parse_flow_draft_object(flow, false)
}

pub(super) fn flow_draft_for_repair(
    flow: &serde_json::Map<String, Value>,
) -> Result<flow::FlowDraft, ToolError> {
    parse_flow_draft_object(flow, true)
}

fn parse_flow_draft_object(
    flow: &serde_json::Map<String, Value>,
    lenient_routes: bool,
) -> Result<flow::FlowDraft, ToolError> {
    let parsed_contracts = optional_array_field(flow, "contracts")?
        .iter()
        .map(parse_flow_contract)
        .collect::<Result<Vec<_>, _>>()?;
    let mut extensions = match flow.get("extensions") {
        Some(value) => string_array(value, "extensions")?,
        None => Vec::new(),
    };
    let mut draft = flow::FlowDraft {
        nodes: optional_array_field(flow, "nodes")?
            .iter()
            .map(parse_flow_node)
            .collect::<Result<Vec<_>, _>>()?,
        contracts: parsed_contracts
            .iter()
            .map(|parsed| parsed.contract.clone())
            .collect(),
        routes: if lenient_routes {
            optional_array_field(flow, "routes")?
                .iter()
                .filter_map(|route| parse_flow_route(route).ok())
                .collect::<Vec<_>>()
        } else {
            optional_array_field(flow, "routes")?
                .iter()
                .map(parse_flow_route)
                .collect::<Result<Vec<_>, _>>()?
        },
        resources: optional_array_field(flow, "resources")?
            .iter()
            .map(parse_flow_resource)
            .collect::<Result<Vec<_>, _>>()?,
        imports: optional_array_field(flow, "imports")?
            .iter()
            .map(parse_flow_import)
            .collect::<Result<Vec<_>, _>>()?,
        policies: parse_flow_policies(flow.get("policies"))?,
        extensions: Vec::new(),
    };
    draft.extensions.append(&mut extensions);
    for parsed in parsed_contracts {
        flow::set_flow_draft_contract_effects(&mut draft, &parsed.contract.id, parsed.effects);
    }
    Ok(draft)
}

pub(super) fn flow_draft_is_empty(draft: &flow::FlowDraft) -> bool {
    draft.nodes.is_empty()
        && draft.contracts.is_empty()
        && draft.routes.is_empty()
        && draft.resources.is_empty()
        && draft.imports.is_empty()
        && draft.policies == flow::FlowPolicies::default()
        && draft.extensions.is_empty()
}

fn parse_flow_node(value: &Value) -> Result<flow::FlowNode, ToolError> {
    match value {
        Value::String(id) => Ok(flow::FlowNode {
            id: id.to_string(),
            ..flow::FlowNode::default()
        }),
        Value::Object(object) => Ok(flow::FlowNode {
            id: string_field(object, &["id"])?.to_string(),
            contract_id: optional_string_field(object, &["contract_id", "contractId"])?
                .map(str::to_string),
            action: object.get("action").map(parse_node_action).transpose()?,
            write_scopes: match object
                .get("write_scopes")
                .or_else(|| object.get("writeScopes"))
            {
                Some(value) => parse_write_scopes(value, "write_scopes")?,
                None => Vec::new(),
            },
            extensions: match object.get("extensions") {
                Some(value) => string_array(value, "extensions")?,
                None => Vec::new(),
            },
        }),
        _ => Err(ToolError::invalid("nodes items must be strings or objects")),
    }
}

fn parse_node_action(value: &Value) -> Result<flow::NodeAction, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("action must be an object"))?;
    Ok(flow::NodeAction {
        driver: parse_node_driver(string_field(object, &["driver"])?)?,
        prompt_ref: optional_string_field(object, &["prompt_ref", "promptRef"])?
            .map(str::to_string),
        resource_refs: optional_string_array_from_object(
            object,
            &["resource_refs", "resourceRefs"],
        )?,
        reads: match object.get("reads") {
            Some(value) => string_array(value, "reads")?,
            None => Vec::new(),
        },
        writes: match object.get("writes") {
            Some(value) => string_array(value, "writes")?,
            None => Vec::new(),
        },
        verdict_artifact: optional_string_field(object, &["verdict_artifact", "verdictArtifact"])?
            .map(str::to_string),
    })
}

fn parse_node_driver(value: &str) -> Result<flow::NodeDriver, ToolError> {
    match value {
        "agent" | "Agent" => Ok(flow::NodeDriver::Agent),
        "script" | "Script" => Ok(flow::NodeDriver::Script),
        "review" | "Review" => Ok(flow::NodeDriver::Review),
        "human" | "Human" => Ok(flow::NodeDriver::Human),
        value => Err(ToolError::invalid(format!(
            "unknown action driver: {value}"
        ))),
    }
}

#[derive(Debug, Clone)]
struct ParsedFlowContract {
    contract: flow::FlowContract,
    effects: Vec<flow::EffectRequirement>,
}

fn parse_flow_contract(value: &Value) -> Result<ParsedFlowContract, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("contracts items must be objects"))?;
    let contract = flow::FlowContract {
        id: string_field(object, &["id"])?.to_string(),
        completion: optional_string_field(object, &["completion"])?
            .map(parse_contract_completion)
            .transpose()?,
        artifacts: optional_array_field(object, "artifacts")?
            .iter()
            .map(parse_contract_artifact)
            .collect::<Result<Vec<_>, _>>()?,
    };
    let effects = optional_array_field(object, "effects")?
        .iter()
        .map(parse_contract_effect)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ParsedFlowContract { contract, effects })
}

fn parse_contract_completion(value: &str) -> Result<flow::ContractCompletion, ToolError> {
    match value {
        "manual" | "Manual" => Ok(flow::ContractCompletion::Manual),
        "all_artifacts" | "allArtifacts" | "AllArtifacts" => {
            Ok(flow::ContractCompletion::AllArtifacts)
        }
        value => Err(ToolError::invalid(format!(
            "unknown contract completion: {value}"
        ))),
    }
}

fn parse_contract_artifact(value: &Value) -> Result<flow::ContractArtifact, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("artifacts items must be objects"))?;
    Ok(flow::ContractArtifact {
        id: string_field(object, &["id"])?.to_string(),
        schema_resource_id: optional_string_field(
            object,
            &["schema_resource_id", "schemaResourceId"],
        )?
        .map(str::to_string),
    })
}

fn parse_contract_effect(value: &Value) -> Result<flow::EffectRequirement, ToolError> {
    match value {
        Value::String(id) => Ok(flow::EffectRequirement {
            id: id.to_string(),
            required: true,
        }),
        Value::Object(object) => Ok(flow::EffectRequirement {
            id: string_field(object, &["id"])?.to_string(),
            required: optional_bool_field(object, &["required"])?.unwrap_or(true),
        }),
        _ => Err(ToolError::invalid(
            "effects items must be strings or objects",
        )),
    }
}

fn parse_flow_route(value: &Value) -> Result<flow::FlowRoute, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("routes items must be objects"))?;
    Ok(flow::FlowRoute {
        predicate: string_field(object, &["predicate"])?.to_string(),
        for_each: optional_string_field(object, &["for_each", "forEach"])?.map(str::to_string),
        activate: string_field(object, &["activate"])?.to_string(),
    })
}

fn parse_flow_resource(value: &Value) -> Result<flow::FlowResource, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("resources items must be objects"))?;
    Ok(flow::FlowResource {
        id: string_field(object, &["id"])?.to_string(),
        kind: parse_resource_kind(string_field(object, &["kind"])?)?,
        source: string_field(object, &["source"])?.to_string(),
    })
}

fn parse_resource_kind(value: &str) -> Result<flow::ResourceKind, ToolError> {
    match value {
        "schema" | "Schema" => Ok(flow::ResourceKind::Schema),
        "rule" | "Rule" => Ok(flow::ResourceKind::Rule),
        "profile" | "Profile" => Ok(flow::ResourceKind::Profile),
        "view" | "View" => Ok(flow::ResourceKind::View),
        "prompt" | "Prompt" => Ok(flow::ResourceKind::Prompt),
        "script" | "Script" => Ok(flow::ResourceKind::Script),
        "flow" | "Flow" => Ok(flow::ResourceKind::Flow),
        "readme" | "README" | "Readme" => Ok(flow::ResourceKind::Readme),
        value => Err(ToolError::invalid(format!(
            "unknown resource kind: {value}"
        ))),
    }
}

fn parse_flow_import(value: &Value) -> Result<flow::FlowImport, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("imports items must be objects"))?;
    Ok(flow::FlowImport {
        resource_id: string_field(object, &["resource_id", "resourceId"])?.to_string(),
        alias: optional_string_field(object, &["alias"])?.map(str::to_string),
    })
}

fn parse_flow_policies(value: Option<&Value>) -> Result<flow::FlowPolicies, ToolError> {
    let Some(value) = value else {
        return Ok(flow::FlowPolicies::default());
    };
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("policies must be an object"))?;
    Ok(flow::FlowPolicies {
        write_scopes: match object
            .get("write_scopes")
            .or_else(|| object.get("writeScopes"))
        {
            Some(value) => parse_write_scopes(value, "write_scopes")?,
            None => Vec::new(),
        },
    })
}

fn parse_write_scopes(value: &Value, name: &str) -> Result<Vec<flow::WriteScope>, ToolError> {
    let scopes = value
        .as_array()
        .ok_or_else(|| ToolError::invalid(format!("{name} must be an array")))?;
    scopes.iter().map(parse_write_scope).collect()
}

fn parse_write_scope(value: &Value) -> Result<flow::WriteScope, ToolError> {
    match value {
        Value::String(value) if value == "workspace" => Ok(flow::WriteScope::Workspace),
        Value::String(value) if value == "system" => Ok(flow::WriteScope::System),
        Value::String(value) => {
            let (kind, id) = value.split_once(':').ok_or_else(|| {
                ToolError::invalid(
                    "write scope must be artifact:<id>, resource:<id>, workspace, or system",
                )
            })?;
            match kind {
                "artifact" if !id.is_empty() => Ok(flow::WriteScope::Artifact(id.to_string())),
                "resource" if !id.is_empty() => Ok(flow::WriteScope::Resource(id.to_string())),
                _ => Err(ToolError::invalid(
                    "write scope must be artifact:<id>, resource:<id>, workspace, or system",
                )),
            }
        }
        Value::Object(object) => {
            let kind = string_field(object, &["kind", "type"])?;
            match kind {
                "artifact" => Ok(flow::WriteScope::Artifact(
                    string_field(object, &["value", "id"])?.to_string(),
                )),
                "resource" => Ok(flow::WriteScope::Resource(
                    string_field(object, &["value", "id"])?.to_string(),
                )),
                "workspace" => Ok(flow::WriteScope::Workspace),
                "system" => Ok(flow::WriteScope::System),
                _ => Err(ToolError::invalid("unknown write scope kind")),
            }
        }
        _ => Err(ToolError::invalid("write scope must be a string or object")),
    }
}
