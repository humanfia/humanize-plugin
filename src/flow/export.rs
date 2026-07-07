use super::*;

pub(super) fn normalized_lock_content(
    draft: &FlowDraft,
    mode: FlowCheckMode,
    diagnostics: &[Diagnostic],
) -> String {
    format!(
        "{{\"mode\":{},\"draft\":{},\"adapter_capabilities\":{},\"node_contracts\":{},\"diagnostics\":{}}}",
        quote(mode.as_str()),
        normalize_draft(draft),
        normalize_adapter_capabilities(&AdapterCapability::from_draft(draft)),
        normalize_node_contracts(&NodeContract::from_draft(draft)),
        normalize_diagnostics(diagnostics)
    )
}

pub(super) fn flow_export(lock: &FlowLock, format: FlowExportFormat) -> String {
    match format {
        FlowExportFormat::Json => export_lock_json(lock),
        FlowExportFormat::Yaml => export_lock_yaml(lock),
    }
}

fn normalize_draft(draft: &FlowDraft) -> String {
    let mut nodes = draft.nodes.clone();
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    let mut contracts = draft.contracts.clone();
    contracts.sort_by(|left, right| left.id.cmp(&right.id));
    let mut routes = draft.routes.clone();
    routes.sort_by(|left, right| {
        left.activate
            .cmp(&right.activate)
            .then(left.predicate.cmp(&right.predicate))
            .then(left.for_each.cmp(&right.for_each))
    });
    let mut resources = draft.resources.clone();
    resources.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.kind.as_str().cmp(right.kind.as_str()))
            .then(left.source.cmp(&right.source))
    });
    let mut imports = draft.imports.clone();
    imports.sort_by(|left, right| {
        left.resource_id
            .cmp(&right.resource_id)
            .then(left.alias.cmp(&right.alias))
    });
    let mut extensions = draft.extensions.clone();
    extensions.sort();

    format!(
        "{{\"nodes\":{},\"contracts\":{},\"routes\":{},\"resources\":{},\"imports\":{},\"policies\":{},\"extensions\":{}}}",
        normalize_nodes(&nodes),
        normalize_contracts(&contracts),
        normalize_routes(&routes),
        normalize_resources(&resources),
        normalize_imports(&imports),
        normalize_policies(&draft.policies),
        normalize_strings(&extensions),
    )
}

fn normalize_nodes(nodes: &[FlowNode]) -> String {
    let values = nodes
        .iter()
        .map(|node| {
            let mut write_scopes = node.write_scopes.clone();
            write_scopes.sort();
            let mut extensions = node.extensions.clone();
            extensions.sort();
            format!(
                "{{\"id\":{},\"contract_id\":{},\"action\":{},\"write_scopes\":{},\"extensions\":{}}}",
                quote(&node.id),
                quote_option(node.contract_id.as_deref()),
                normalize_action(node.action.as_ref()),
                normalize_write_scopes(&write_scopes),
                normalize_strings(&extensions)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_action(action: Option<&NodeAction>) -> String {
    let Some(action) = action else {
        return "null".into();
    };
    let mut resource_refs = action.resource_refs.clone();
    resource_refs.sort();
    let mut reads = action.reads.clone();
    reads.sort();
    let mut writes = action.writes.clone();
    writes.sort();

    format!(
        "{{\"driver\":{},\"prompt_ref\":{},\"resource_refs\":{},\"reads\":{},\"writes\":{},\"verdict_artifact\":{}}}",
        quote(action.driver.as_str()),
        quote_option(action.prompt_ref.as_deref()),
        normalize_strings(&resource_refs),
        normalize_strings(&reads),
        normalize_strings(&writes),
        quote_option(action.verdict_artifact.as_deref())
    )
}

fn normalize_adapter_capabilities(capabilities: &[AdapterCapability]) -> String {
    let mut capabilities = capabilities.to_vec();
    capabilities.sort_by(|left, right| {
        left.node_id
            .cmp(&right.node_id)
            .then(left.driver.as_str().cmp(right.driver.as_str()))
    });
    let values = capabilities
        .iter()
        .map(|capability| {
            let mut requires = capability.requires.clone();
            let mut prefers = capability.prefers.clone();
            let mut accepts = capability.accepts.clone();
            requires.sort();
            prefers.sort();
            accepts.sort();
            format!(
                "{{\"node_id\":{},\"driver\":{},\"requires\":{},\"prefers\":{},\"accepts\":{}}}",
                quote(&capability.node_id),
                quote(capability.driver.as_str()),
                normalize_strings(&requires),
                normalize_strings(&prefers),
                normalize_strings(&accepts),
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_node_contracts(contracts: &[NodeContract]) -> String {
    let mut contracts = contracts.to_vec();
    contracts.sort_by(|left, right| {
        left.node_id
            .cmp(&right.node_id)
            .then(left.contract_id.cmp(&right.contract_id))
    });
    let values = contracts
        .iter()
        .map(|contract| {
            let mut requires = contract.requires.clone();
            let mut prefers = contract.prefers.clone();
            let mut accepts = contract.accepts.clone();
            let mut artifacts = contract.artifact_requirements.clone();
            let mut effects = contract.effect_requirements.clone();
            requires.sort();
            prefers.sort();
            accepts.sort();
            artifacts.sort();
            effects.sort();
            format!(
                "{{\"node_id\":{},\"contract_id\":{},\"requires\":{},\"prefers\":{},\"accepts\":{},\"completion_policy\":{},\"artifact_requirements\":{},\"effect_requirements\":{},\"stop_gate\":{}}}",
                quote(&contract.node_id),
                quote_option(contract.contract_id.as_deref()),
                normalize_strings(&requires),
                normalize_strings(&prefers),
                normalize_strings(&accepts),
                quote(contract.completion_policy.as_str()),
                normalize_artifact_requirements(&artifacts),
                normalize_effect_requirements(&effects),
                quote(contract.stop_gate.as_str()),
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_artifact_requirements(requirements: &[ArtifactRequirement]) -> String {
    let values = requirements
        .iter()
        .map(|requirement| {
            format!(
                "{{\"id\":{},\"schema_resource_id\":{},\"required\":{}}}",
                quote(&requirement.id),
                quote_option(requirement.schema_resource_id.as_deref()),
                requirement.required,
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_effect_requirements(requirements: &[EffectRequirement]) -> String {
    let values = requirements
        .iter()
        .map(|requirement| {
            format!(
                "{{\"id\":{},\"required\":{}}}",
                quote(&requirement.id),
                requirement.required,
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_contracts(contracts: &[FlowContract]) -> String {
    let values = contracts
        .iter()
        .map(|contract| {
            let mut artifacts = contract.artifacts.clone();
            artifacts.sort_by(|left, right| {
                left.id
                    .cmp(&right.id)
                    .then(left.schema_resource_id.cmp(&right.schema_resource_id))
            });
            format!(
                "{{\"id\":{},\"completion\":{},\"artifacts\":{}}}",
                quote(&contract.id),
                contract
                    .completion
                    .as_ref()
                    .map(ContractCompletion::as_str)
                    .map(quote)
                    .unwrap_or_else(|| "null".into()),
                normalize_artifacts(&artifacts)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_artifacts(artifacts: &[ContractArtifact]) -> String {
    let values = artifacts
        .iter()
        .map(|artifact| {
            format!(
                "{{\"id\":{},\"schema_resource_id\":{}}}",
                quote(&artifact.id),
                quote_option(artifact.schema_resource_id.as_deref())
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_routes(routes: &[FlowRoute]) -> String {
    let values = routes
        .iter()
        .map(|route| {
            format!(
                "{{\"activate\":{},\"predicate\":{},\"for_each\":{}}}",
                quote(&route.activate),
                quote(&route.predicate),
                quote_option(route.for_each.as_deref())
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_resources(resources: &[FlowResource]) -> String {
    let values = resources
        .iter()
        .map(|resource| {
            format!(
                "{{\"id\":{},\"kind\":{},\"source\":{}}}",
                quote(&resource.id),
                quote(resource.kind.as_str()),
                quote(&resource.source)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_imports(imports: &[FlowImport]) -> String {
    let values = imports
        .iter()
        .map(|import| {
            format!(
                "{{\"resource_id\":{},\"alias\":{}}}",
                quote(&import.resource_id),
                quote_option(import.alias.as_deref())
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_policies(policies: &FlowPolicies) -> String {
    let mut write_scopes = policies.write_scopes.clone();
    write_scopes.sort();
    format!(
        "{{\"write_scopes\":{}}}",
        normalize_write_scopes(&write_scopes)
    )
}

fn normalize_write_scopes(write_scopes: &[WriteScope]) -> String {
    let values = write_scopes
        .iter()
        .map(|scope| {
            format!(
                "{{\"kind\":{},\"value\":{}}}",
                quote(scope.tag()),
                quote_option(scope.value())
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_diagnostics(diagnostics: &[Diagnostic]) -> String {
    let mut sorted = diagnostics.to_vec();
    sorted.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then(left.domain.cmp(&right.domain))
            .then(left.severity_level.cmp(&right.severity_level))
            .then(left.location.cmp(&right.location))
            .then(left.message.cmp(&right.message))
            .then(left.fix_hint.cmp(&right.fix_hint))
            .then(left.why_it_matters.cmp(&right.why_it_matters))
            .then(left.repair_kinds.cmp(&right.repair_kinds))
    });

    let values = sorted.iter().map(normalize_diagnostic).collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_diagnostic(diagnostic: &Diagnostic) -> String {
    let repair_kinds = diagnostic
        .repair_kinds
        .iter()
        .map(|kind| kind.as_str().to_string())
        .collect::<Vec<_>>();
    format!(
        "{{\"code\":{},\"domain\":{},\"severity\":{},\"legacy_severity\":{},\"repairability\":{},\"location\":{},\"message\":{},\"fix_hint\":{},\"why_it_matters\":{},\"repair_kinds\":{}}}",
        quote(&diagnostic.code),
        quote(diagnostic.domain.as_str()),
        quote(diagnostic.severity_level.as_str()),
        quote(diagnostic.severity.as_str()),
        quote(diagnostic.repairability.as_str()),
        quote(&diagnostic.location),
        quote(&diagnostic.message),
        quote_option(diagnostic.fix_hint.as_deref()),
        quote_option(diagnostic.why_it_matters.as_deref()),
        normalize_strings(&repair_kinds),
    )
}

fn normalize_strings(values: &[String]) -> String {
    let quoted = values.iter().map(|value| quote(value)).collect::<Vec<_>>();
    format!("[{}]", quoted.join(","))
}

fn export_lock_json(lock: &FlowLock) -> String {
    format!(
        "{{\n  \"id\": {},\n  \"check_mode\": {},\n  \"diagnostics\": {},\n  \"content\": {}\n}}",
        quote(&lock.id),
        quote(lock.mode.as_str()),
        normalize_diagnostics(&lock.diagnostics),
        quote(&lock.normalized_content)
    )
}

fn export_lock_yaml(lock: &FlowLock) -> String {
    let diagnostics = if lock.diagnostics.is_empty() {
        "[]".into()
    } else {
        lock.diagnostics
            .iter()
            .map(|diagnostic| {
                let repair_kinds = diagnostic
                    .repair_kinds
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "  - code: {}\n    domain: {}\n    severity: {}\n    legacy_severity: {}\n    repairability: {}\n    location: {}\n    message: {}\n    fix_hint: {}\n    why_it_matters: {}\n    repair_kinds: {}",
                    yaml_scalar(&diagnostic.code),
                    yaml_scalar(diagnostic.domain.as_str()),
                    yaml_scalar(diagnostic.severity_level.as_str()),
                    yaml_scalar(diagnostic.severity.as_str()),
                    yaml_scalar(diagnostic.repairability.as_str()),
                    yaml_scalar(&diagnostic.location),
                    yaml_scalar(&diagnostic.message),
                    diagnostic
                        .fix_hint
                        .as_deref()
                        .map(yaml_scalar)
                        .unwrap_or_else(|| "null".into()),
                    diagnostic
                        .why_it_matters
                        .as_deref()
                        .map(yaml_scalar)
                        .unwrap_or_else(|| "null".into()),
                    if repair_kinds.is_empty() {
                        "[]".into()
                    } else {
                        yaml_scalar(&repair_kinds)
                    },
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "id: {}\ncheck_mode: {}\ndiagnostics: {}\ncontent: {}\n",
        lock.id,
        lock.mode.as_str(),
        diagnostics,
        yaml_scalar(&lock.normalized_content)
    )
}

fn quote_option(value: Option<&str>) -> String {
    value.map(quote).unwrap_or_else(|| "null".into())
}

fn quote(value: &str) -> String {
    format!("\"{}\"", escape_json(value))
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn yaml_scalar(value: &str) -> String {
    quote(value)
}

pub(super) fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
