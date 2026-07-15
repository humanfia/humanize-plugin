use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

use super::*;

pub fn flow_repair(input: &FlowRepairInput) -> FlowRepairReport {
    let mut diagnostics = flow_check(&input.draft, input.mode).diagnostics;
    diagnostics.extend(input.diagnostics.clone());
    let diagnostics = repair_diagnostics(diagnostics, input.include_warnings);
    let candidates = repair_candidates(&input.draft, &diagnostics);

    FlowRepairReport {
        diagnostics,
        candidates,
    }
}

fn repair_diagnostics(diagnostics: Vec<Diagnostic>, include_warnings: bool) -> Vec<Diagnostic> {
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Fatal)
    {
        return diagnostics
            .into_iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Fatal)
            .collect();
    }

    diagnostics
        .into_iter()
        .filter(|diagnostic| include_warnings || diagnostic.severity != Severity::Warning)
        .collect()
}

fn repair_candidates(draft: &FlowDraft, diagnostics: &[Diagnostic]) -> Vec<FlowRepairCandidate> {
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Fatal)
    {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    for diagnostic in diagnostics {
        if diagnostic.severity != Severity::Error
            || diagnostic.code != "FLOW_UNKNOWN_ROUTE_TARGET"
            || diagnostic.repairability != Repairability::Candidate
            || !diagnostic
                .repair_kinds
                .contains(&RepairKind::AddRouteTarget)
        {
            continue;
        }

        candidates.extend(
            draft
                .nodes
                .iter()
                .filter(|node| !node.id.trim().is_empty())
                .map(|node| FlowRepairCandidate {
                    repair_kind: RepairKind::AddRouteTarget,
                    location: diagnostic.location.clone(),
                    replacement: node.id.clone(),
                }),
        );
    }
    candidates
}

pub fn flow_check(draft: &FlowDraft, mode: FlowCheckMode) -> CheckReport {
    let mut diagnostics = Vec::new();
    let mut seen_node_ids = HashSet::new();
    for (index, node) in draft.nodes.iter().enumerate() {
        if !seen_node_ids.insert(node.id.as_str()) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Contract,
                "FLOW_DUPLICATE_NODE_ID",
                format!("nodes[{index}].id"),
                format!("node id '{}' is declared more than once", node.id),
                "Keep exactly one node for each id.",
                "A route target and runtime activation must resolve to one canonical node.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }
    }
    let mut seen_contract_ids = HashSet::new();
    for (index, contract) in draft.contracts.iter().enumerate() {
        if !seen_contract_ids.insert(contract.id.as_str()) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Contract,
                "FLOW_DUPLICATE_CONTRACT_ID",
                format!("contracts[{index}].id"),
                format!("contract id '{}' is declared more than once", contract.id),
                "Keep exactly one contract for each id.",
                "A node contract reference must resolve to one canonical contract.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }
        let mut artifact_ids = HashSet::new();
        for (artifact_index, artifact) in contract.artifacts.iter().enumerate() {
            if !artifact_ids.insert(artifact.id.as_str()) {
                diagnostics.push(Diagnostic::error(
                    DiagnosticDomain::Contract,
                    "FLOW_DUPLICATE_CONTRACT_ARTIFACT",
                    format!("contracts[{index}].artifacts[{artifact_index}].id"),
                    format!(
                        "contract '{}' declares artifact '{}' more than once",
                        contract.id, artifact.id
                    ),
                    "Keep exactly one artifact requirement for each id in a contract.",
                    "A stop contract must have one requirement for each artifact key.",
                    DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
                ));
            }
        }
    }
    let node_ids = draft
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<HashSet<_>>();
    let contract_ids = draft
        .contracts
        .iter()
        .map(|contract| contract.id.as_str())
        .collect::<HashSet<_>>();
    let schema_ids = draft
        .resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::Schema)
        .map(|resource| resource.id.as_str())
        .collect::<HashSet<_>>();
    let resource_by_id = draft
        .resources
        .iter()
        .map(|resource| (resource.id.as_str(), &resource.kind))
        .collect::<HashMap<_, _>>();
    let mut resource_paths = HashSet::new();
    for (index, resource) in draft.resources.iter().enumerate() {
        if !package_path_is_valid(&resource.id) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Package,
                "FLOW_INVALID_RESOURCE_PATH",
                format!("resources[{index}].path"),
                format!(
                    "resource path '{}' is not a safe package-relative path",
                    resource.id
                ),
                "Use a non-empty relative path without '.', '..', or an absolute prefix.",
                "Locked packages may contain only files rooted inside the package directory.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }
        if !resource_paths.insert(resource.id.as_str()) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Package,
                "FLOW_DUPLICATE_RESOURCE_PATH",
                format!("resources[{index}].path"),
                format!("resource path '{}' is declared more than once", resource.id),
                "Keep exactly one embedded file for each package-relative path.",
                "A package file must have one content owner.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }
        if resource.kind == ResourceKind::Readme && resource.id != "README.md" {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Package,
                "FLOW_INVALID_README_PATH",
                format!("resources[{index}].path"),
                "the package README must use the root path 'README.md'",
                "Use exactly one readme resource at README.md.",
                "The package root README is the single human-authored description supplied to consumers.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }
        if resource.id == "README.md" && resource.kind != ResourceKind::Readme {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Package,
                "FLOW_INVALID_README_PATH",
                format!("resources[{index}].kind"),
                "the package root README.md must use resource kind 'readme'",
                "Set the README.md resource kind to readme.",
                "The package root README must have one unambiguous semantic owner.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }
        if resource.kind == ResourceKind::Skill && !skill_path_is_valid(&resource.id) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Package,
                "FLOW_INVALID_SKILL_PATH",
                format!("resources[{index}].path"),
                "skill resources must use 'skills/<name>/SKILL.md'",
                "Place each skill in one package-relative skills/<name>/SKILL.md path.",
                "Skill distribution relies on one canonical file path per skill.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }
    }

    for (index, import) in draft.imports.iter().enumerate() {
        if !resource_by_id.contains_key(import.resource_id.as_str()) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Resource,
                "FLOW_UNRESOLVED_IMPORT",
                format!("imports[{index}].resource_id"),
                format!(
                    "flow import references missing embedded resource '{}'",
                    import.resource_id
                ),
                "Embed the referenced resource or remove the import.",
                "A locked package cannot depend on unresolved external resources.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }
    }

    if draft_is_non_empty_package(draft) {
        if !draft_has_readme(draft) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Package,
                "FLOW_MISSING_README",
                "resources",
                "non-empty flow packages must include a README resource",
                "Add a resource with kind 'readme' that describes the package.",
                "Packages need human-readable context before they can be shared or executed safely.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        } else if !draft_has_readme_content(draft) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Package,
                "FLOW_EMPTY_README",
                "resources",
                "README resources must include explanatory content",
                "Add non-empty content to at least one README resource.",
                "README content is descriptive substance and cannot be inferred mechanically.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }
    }

    if flow_draft_qos(draft)
        .completion_target
        .as_deref()
        .is_some_and(|target| target.trim().is_empty())
    {
        diagnostics.push(Diagnostic::error(
            DiagnosticDomain::Policy,
            "FLOW_EMPTY_QOS_COMPLETION_TARGET",
            "qos.completion_target",
            "QoS completion target must not be empty",
            "Remove completion_target for open-ended work or provide a non-empty artifact, board, or time target.",
            "Consumers need a meaningful target when QoS declares one.",
            DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
        ));
    }

    let mut first_route_by_identity = HashMap::new();
    for (index, route) in draft.routes.iter().enumerate() {
        let route_identity = canonical_route_identity(route);
        if let Some(first_index) = first_route_by_identity.insert(route_identity, index) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Route,
                "FLOW_DUPLICATE_ROUTE",
                format!("routes[{index}]"),
                format!("route duplicates routes[{first_index}]"),
                "Remove the duplicate route.",
                "A canonical route may fire only once for each trigger fact version.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }
        if !node_ids.contains(route.activate.as_str()) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Route,
                "FLOW_UNKNOWN_ROUTE_TARGET",
                format!("routes[{index}].activate"),
                format!(
                    "route activate target '{}' does not match a draft node",
                    route.activate
                ),
                "Add the target node or update the route target.",
                "Routes can only activate nodes that exist in the draft.",
                DiagnosticRepair::new(Repairability::Candidate, vec![RepairKind::AddRouteTarget]),
            ));
        }
    }

    for contract in &draft.contracts {
        if contract.completion.is_none() {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Contract,
                "FLOW_MISSING_CONTRACT_COMPLETION",
                format!("contracts[{}].completion", contract.id),
                format!("contract '{}' has no completion rule", contract.id),
                "Set a completion rule such as Manual or AllArtifacts.",
                "Completion policy defines when a node contract can be considered satisfied.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }

        for artifact in &contract.artifacts {
            match artifact.schema_resource_id.as_deref() {
                Some(schema_id) if schema_ids.contains(schema_id) => {}
                Some(schema_id) => diagnostics.push(Diagnostic::error(
                    DiagnosticDomain::Resource,
                    "FLOW_MISSING_ARTIFACT_SCHEMA",
                    format!(
                        "contracts[{}].artifacts[{}].schema_resource_id",
                        contract.id, artifact.id
                    ),
                    format!(
                        "artifact '{}' references missing schema resource '{}'",
                        artifact.id, schema_id
                    ),
                    "Add a schema resource or update the artifact schema reference.",
                    "Artifact schemas make delivered data inspectable across nodes.",
                    DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
                )),
                None => diagnostics.push(Diagnostic::error(
                    DiagnosticDomain::Resource,
                    "FLOW_MISSING_ARTIFACT_SCHEMA",
                    format!("contracts[{}].artifacts[{}]", contract.id, artifact.id),
                    format!("artifact '{}' has no schema resource", artifact.id),
                    "Attach the artifact to a schema resource.",
                    "Artifact schemas make delivered data inspectable across nodes.",
                    DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
                )),
            }
        }
    }

    for (index, extension) in draft.extensions.iter().enumerate() {
        if !is_authoring_extension_kind(extension) {
            diagnostics.push(Diagnostic::fatal(
                DiagnosticDomain::Policy,
                "FLOW_AUTHORING_PRIMITIVE_MISUSE",
                format!("extensions[{index}]"),
                format!("'{extension}' is not a flow authoring primitive"),
                "Represent execution details outside FlowDraft authoring data.",
                "Runtime primitives in authoring data can change execution semantics.",
                DiagnosticRepair::new(Repairability::None, Vec::new()),
            ));
        }
    }
    for node in &draft.nodes {
        if let Some(contract_id) = node.contract_id.as_deref()
            && !contract_ids.contains(contract_id)
        {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Contract,
                "FLOW_UNKNOWN_NODE_CONTRACT",
                format!("nodes[{}].contract_id", node.id),
                format!(
                    "node '{}' references missing contract '{}'",
                    node.id, contract_id
                ),
                "Add the contract or update the node contract_id.",
                "Node contract references must resolve before runtime stop contracts can be derived.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
            ));
        }

        if let Some(action) = &node.action {
            if action.driver == NodeDriver::Script {
                diagnostics.push(Diagnostic::error(
                    DiagnosticDomain::RuntimeCompat,
                    "FLOW_UNSUPPORTED_SCRIPT_ACTION_DRIVER",
                    format!("nodes[{}].action.driver", node.id),
                    "script action drivers are not supported by autonomous tmux actuation",
                    "Use an agent or review action with explicit prompt and artifact contracts.",
                    "Runnable flows must not lock nodes that the runtime cannot autonomously actuate.",
                    DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
                ));
            }

            if let Some(prompt_ref) = &action.prompt_ref {
                match resource_by_id.get(prompt_ref.as_str()) {
                    Some(ResourceKind::Prompt) => {}
                    Some(_) => diagnostics.push(Diagnostic::error(
                        DiagnosticDomain::Resource,
                        "FLOW_INVALID_ACTION_PROMPT",
                        format!("nodes[{}].action.prompt_ref", node.id),
                        format!(
                            "node action prompt_ref '{prompt_ref}' must reference a prompt resource"
                        ),
                        "Use a resource with kind 'prompt' for prompt_ref.",
                        "Prompt references need a prompt resource so adapters receive the intended instruction.",
                        DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
                    )),
                    None => diagnostics.push(Diagnostic::error(
                        DiagnosticDomain::Resource,
                        "FLOW_UNKNOWN_ACTION_PROMPT",
                        format!("nodes[{}].action.prompt_ref", node.id),
                        format!("node action references missing prompt resource '{prompt_ref}'"),
                        "Add the prompt resource or update the action prompt reference.",
                        "Prompt references need a resolvable resource before the node can run.",
                        DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
                    )),
                }
            }

            for (index, resource_id) in action.resource_refs.iter().enumerate() {
                if !resource_by_id.contains_key(resource_id.as_str()) {
                    diagnostics.push(Diagnostic::error(
                        DiagnosticDomain::Resource,
                        "FLOW_UNKNOWN_ACTION_RESOURCE",
                        format!("nodes[{}].action.resource_refs[{}]", node.id, index),
                        format!("node action references missing resource '{resource_id}'"),
                        "Add the resource or update the action resource reference.",
                        "Action resources must exist before adapters can mount or read them.",
                        DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
                    ));
                }
            }

            if action
                .verdict_artifact
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                diagnostics.push(Diagnostic::error(
                    DiagnosticDomain::Contract,
                    "FLOW_EMPTY_ACTION_VERDICT_ARTIFACT",
                    format!("nodes[{}].action.verdict_artifact", node.id),
                    "action verdict artifact must not be empty",
                    "Use a non-empty artifact-like id such as artifact.verdict.",
                    "Verdict artifacts need stable ids so downstream routes and contracts can reference them.",
                    DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
                ));
            }
        }

        for (index, extension) in node.extensions.iter().enumerate() {
            if !is_authoring_extension_kind(extension) {
                diagnostics.push(Diagnostic::fatal(
                    DiagnosticDomain::Policy,
                    "FLOW_AUTHORING_PRIMITIVE_MISUSE",
                    format!("nodes[{}].extensions[{}]", node.id, index),
                    format!("'{extension}' is not a flow authoring primitive"),
                    "Represent execution details outside FlowDraft authoring data.",
                    "Runtime primitives in authoring data can change execution semantics.",
                    DiagnosticRepair::new(Repairability::None, Vec::new()),
                ));
            }
        }
    }

    let broad_write_severity = match mode {
        FlowCheckMode::Core => Severity::Warning,
        FlowCheckMode::Strict => Severity::Error,
    };
    for (index, scope) in draft.policies.write_scopes.iter().enumerate() {
        push_broad_write_scope_diagnostic(
            scope,
            broad_write_severity,
            format!("policies.write_scopes[{index}]"),
            &mut diagnostics,
        );
    }
    for node in &draft.nodes {
        for (index, scope) in node.write_scopes.iter().enumerate() {
            push_broad_write_scope_diagnostic(
                scope,
                broad_write_severity,
                format!("nodes[{}].write_scopes[{}]", node.id, index),
                &mut diagnostics,
            );
        }
    }

    CheckReport { mode, diagnostics }
}

pub fn flow_check_run_compatibility(
    draft: &FlowDraft,
    input: RunCompatibility,
) -> RunCompatibilityResult {
    let available = input
        .available_resources
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let diagnostics = draft
        .resources
        .iter()
        .filter(|resource| !available.contains(resource.id.as_str()))
        .map(|resource| {
            Diagnostic::error(
                DiagnosticDomain::RuntimeCompat,
                "FLOW_RUN_RESOURCE_UNAVAILABLE",
                format!("resources[{}]", resource.id),
                format!("resource '{}' is not available to the run", resource.id),
                "Provide the resource to the run or remove it from the draft.",
                "Runs can only execute drafts whose resources are present in the runtime context.",
                DiagnosticRepair::new(
                    Repairability::GuidanceOnly,
                    vec![RepairKind::ProvideRuntimeResource],
                ),
            )
        })
        .collect::<Vec<_>>();

    RunCompatibilityResult {
        compatible: diagnostics.is_empty(),
        diagnostics,
    }
}

pub(super) fn draft_is_non_empty_package(draft: &FlowDraft) -> bool {
    !draft.nodes.is_empty()
        || !draft.contracts.is_empty()
        || !draft.routes.is_empty()
        || !draft.resources.is_empty()
        || !draft.imports.is_empty()
        || !draft.policies.write_scopes.is_empty()
        || !flow_draft_qos(draft).is_default()
        || !draft.extensions.is_empty()
}

pub(super) fn draft_has_readme(draft: &FlowDraft) -> bool {
    draft
        .resources
        .iter()
        .any(|resource| resource.kind == ResourceKind::Readme && resource.id == "README.md")
}

pub(super) fn draft_has_readme_content(draft: &FlowDraft) -> bool {
    draft
        .resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::Readme && resource.id == "README.md")
        .any(|resource| !resource.source.trim().is_empty())
}

pub(super) fn package_path_is_valid(value: &str) -> bool {
    !value.is_empty()
        && value != "flow.json"
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub(super) fn skill_path_is_valid(value: &str) -> bool {
    let components = Path::new(value).components().collect::<Vec<_>>();
    matches!(
        components.as_slice(),
        [Component::Normal(root), Component::Normal(name), Component::Normal(file)]
            if *root == "skills"
                && !name.is_empty()
                && *file == "SKILL.md"
    )
}
