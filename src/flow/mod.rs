use std::collections::{HashMap, HashSet};

mod export;

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FlowDraft {
    pub nodes: Vec<FlowNode>,
    pub contracts: Vec<FlowContract>,
    pub routes: Vec<FlowRoute>,
    pub resources: Vec<FlowResource>,
    pub imports: Vec<FlowImport>,
    pub policies: FlowPolicies,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FlowNode {
    pub id: String,
    pub contract_id: Option<String>,
    pub action: Option<NodeAction>,
    pub write_scopes: Vec<WriteScope>,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NodeAction {
    pub driver: NodeDriver,
    pub prompt_ref: Option<String>,
    pub resource_refs: Vec<String>,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub verdict_artifact: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NodeDriver {
    Agent,
    Script,
    Review,
    Human,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowContract {
    pub id: String,
    pub completion: Option<ContractCompletion>,
    pub artifacts: Vec<ContractArtifact>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ContractCompletion {
    Manual,
    AllArtifacts,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ContractArtifact {
    pub id: String,
    pub schema_resource_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowRoute {
    pub predicate: String,
    pub for_each: Option<String>,
    pub activate: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowResource {
    pub id: String,
    pub kind: ResourceKind,
    pub source: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ResourceKind {
    Schema,
    Rule,
    Profile,
    View,
    Prompt,
    Script,
    Flow,
    Readme,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowImport {
    pub resource_id: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FlowPolicies {
    pub write_scopes: Vec<WriteScope>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AdapterCapability {
    pub node_id: String,
    pub driver: NodeDriver,
    pub requires: Vec<String>,
    pub prefers: Vec<String>,
    pub accepts: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NodeContract {
    pub node_id: String,
    pub contract_id: Option<String>,
    pub requires: Vec<String>,
    pub prefers: Vec<String>,
    pub accepts: Vec<String>,
    pub completion_policy: CompletionPolicy,
    pub artifact_requirements: Vec<ArtifactRequirement>,
    pub effect_requirements: Vec<EffectRequirement>,
    pub stop_gate: StopGate,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum CompletionPolicy {
    #[default]
    None,
    Manual,
    AllArtifacts,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct ArtifactRequirement {
    pub id: String,
    pub schema_resource_id: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct EffectRequirement {
    pub id: String,
    pub required: bool,
}

const CONTRACT_EFFECTS_EXTENSION_PREFIX: &str = "humanize.contract_effects:";

pub fn flow_draft_contract_effects(draft: &FlowDraft, contract_id: &str) -> Vec<EffectRequirement> {
    draft
        .extensions
        .iter()
        .filter_map(|extension| parse_contract_effects_extension(extension, contract_id))
        .flatten()
        .collect()
}

pub fn set_flow_draft_contract_effects(
    draft: &mut FlowDraft,
    contract_id: &str,
    effects: Vec<EffectRequirement>,
) {
    draft
        .extensions
        .retain(|extension| !extension_is_for_contract_effects(extension, contract_id));
    if effects.is_empty() {
        return;
    }
    let effects = effects
        .into_iter()
        .map(|effect| {
            serde_json::json!({
                "id": effect.id,
                "required": effect.required,
            })
        })
        .collect::<Vec<_>>();
    let payload = serde_json::json!({
        "contract_id": contract_id,
        "effects": effects,
    });
    draft.extensions.push(format!(
        "{}{}",
        CONTRACT_EFFECTS_EXTENSION_PREFIX,
        serde_json::to_string(&payload).expect("contract effects extension should serialize")
    ));
}

fn extension_is_for_contract_effects(extension: &str, contract_id: &str) -> bool {
    parse_contract_effects_extension(extension, contract_id).is_some()
}

fn parse_contract_effects_extension(
    extension: &str,
    contract_id: &str,
) -> Option<Vec<EffectRequirement>> {
    let payload = extension.strip_prefix(CONTRACT_EFFECTS_EXTENSION_PREFIX)?;
    let value = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    let object = value.as_object()?;
    if object.get("contract_id")?.as_str()? != contract_id {
        return None;
    }
    let effects = object
        .get("effects")?
        .as_array()?
        .iter()
        .filter_map(|effect| {
            let object = effect.as_object()?;
            Some(EffectRequirement {
                id: object.get("id")?.as_str()?.to_string(),
                required: object
                    .get("required")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
            })
        })
        .collect::<Vec<_>>();
    Some(effects)
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum StopGate {
    Required,
    Preferred,
    #[default]
    None,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum WriteScope {
    Artifact(String),
    Resource(String),
    Workspace,
    System,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum FlowCheckMode {
    #[default]
    Core,
    Strict,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Diagnostic {
    pub code: String,
    pub domain: DiagnosticDomain,
    pub severity: Severity,
    pub severity_level: DiagnosticSeverity,
    pub repairability: Repairability,
    pub location: String,
    pub message: String,
    pub fix_hint: Option<String>,
    pub why_it_matters: Option<String>,
    pub repair_kinds: Vec<RepairKind>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum DiagnosticDomain {
    Package,
    Contract,
    Resource,
    Route,
    Policy,
    RuntimeCompat,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum DiagnosticSeverity {
    Fatal,
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum Repairability {
    Automatic,
    Candidate,
    GuidanceOnly,
    None,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum RepairKind {
    RouteWhenToPredicate,
    RouteToToActivate,
    RouteArtifactObjectToExists,
    RouteBareArtifactDeliveredToExists,
    AddRouteTarget,
    AddReadmeResource,
    GenerateReadme,
    AddArtifactSchema,
    AddContractCompletion,
    NarrowWriteScope,
    ProvideRuntimeResource,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct CheckReport {
    pub mode: FlowCheckMode,
    pub diagnostics: Vec<Diagnostic>,
}

impl CheckReport {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.severity_level,
                DiagnosticSeverity::Fatal | DiagnosticSeverity::Error
            )
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunCompatibility {
    pub available_resources: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunCompatibilityResult {
    pub compatible: bool,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowLock {
    id: String,
    mode: FlowCheckMode,
    diagnostics: Vec<Diagnostic>,
    draft: FlowDraft,
    normalized_content: String,
}

impl FlowLock {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn mode(&self) -> FlowCheckMode {
        self.mode
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn draft(&self) -> &FlowDraft {
        &self.draft
    }

    pub fn normalized_content(&self) -> &str {
        &self.normalized_content
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowLockError {
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FlowRepairInput {
    pub draft: FlowDraft,
    pub mode: FlowCheckMode,
    pub route_authoring: Vec<RouteAuthoring>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RouteAuthoring {
    pub when: Option<String>,
    pub predicate: Option<RoutePredicateDraft>,
    pub to: Option<String>,
    pub activate: Option<String>,
    pub for_each: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RoutePredicateDraft {
    Text(String),
    Artifact(String),
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FlowRepairReport {
    pub diagnostics: Vec<Diagnostic>,
    pub patches: Vec<FlowRepairPatch>,
    pub candidates: Vec<FlowRepairCandidate>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowRepairPatch {
    pub repair_kind: RepairKind,
    pub location: String,
    pub replacement: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowRepairCandidate {
    pub repair_kind: RepairKind,
    pub location: String,
    pub replacement: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FlowExportFormat {
    Json,
    Yaml,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FlowSuggestInput {
    pub goal: String,
    pub nodes: Vec<String>,
    pub artifact: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowSuggestError {
    message: String,
}

impl FlowSuggestError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

pub fn flow_suggest(input: FlowSuggestInput) -> Result<FlowDraft, FlowSuggestError> {
    let goal = input.goal.trim();
    if goal.is_empty() {
        return Err(FlowSuggestError {
            message: "goal must not be blank".into(),
        });
    }

    let artifact = input
        .artifact
        .as_deref()
        .map(|value| slug_ascii_id(value, "result"))
        .unwrap_or_else(|| "result".into());
    let raw_nodes = if input.nodes.is_empty() {
        vec!["root".to_string()]
    } else {
        input.nodes
    };
    let node_ids = unique_ascii_ids(&raw_nodes, "node");

    let nodes = node_ids
        .iter()
        .map(|node_id| FlowNode {
            id: node_id.clone(),
            contract_id: Some(format!("contract.{node_id}")),
            ..FlowNode::default()
        })
        .collect::<Vec<_>>();
    let contracts = node_ids
        .iter()
        .map(|node_id| FlowContract {
            id: format!("contract.{node_id}"),
            completion: Some(ContractCompletion::AllArtifacts),
            artifacts: vec![ContractArtifact {
                id: artifact.clone(),
                schema_resource_id: Some(format!("schema.{node_id}.{artifact}")),
            }],
        })
        .collect::<Vec<_>>();
    let mut resources = vec![FlowResource {
        id: "readme.main".into(),
        kind: ResourceKind::Readme,
        source: format!("inline:{goal}"),
    }];
    resources.extend(node_ids.iter().map(|node_id| FlowResource {
        id: format!("schema.{node_id}.{artifact}"),
        kind: ResourceKind::Schema,
        source: format!("inline:{artifact}"),
    }));

    Ok(FlowDraft {
        nodes,
        contracts,
        routes: Vec::new(),
        resources,
        imports: Vec::new(),
        policies: FlowPolicies::default(),
        extensions: Vec::new(),
    })
}

impl AdapterCapability {
    pub fn from_action(node_id: impl Into<String>, action: &NodeAction) -> Self {
        let (requires, prefers, accepts) = action_contract_fields(action);

        Self {
            node_id: node_id.into(),
            driver: action.driver,
            requires,
            prefers,
            accepts,
        }
    }

    pub fn from_draft(draft: &FlowDraft) -> Vec<Self> {
        draft
            .nodes
            .iter()
            .filter_map(|node| {
                node.action
                    .as_ref()
                    .map(|action| Self::from_action(node.id.clone(), action))
            })
            .collect()
    }
}

impl NodeContract {
    pub fn from_draft(draft: &FlowDraft) -> Vec<Self> {
        let contracts_by_id = draft
            .contracts
            .iter()
            .map(|contract| (contract.id.as_str(), contract))
            .collect::<HashMap<_, _>>();

        draft
            .nodes
            .iter()
            .map(|node| {
                let contract = node
                    .contract_id
                    .as_deref()
                    .and_then(|contract_id| contracts_by_id.get(contract_id).copied());
                let (requires, prefers, accepts) = node
                    .action
                    .as_ref()
                    .map(action_contract_fields)
                    .unwrap_or_else(|| (Vec::new(), Vec::new(), Vec::new()));
                let artifact_requirements = contract
                    .map(|contract| {
                        contract
                            .artifacts
                            .iter()
                            .map(|artifact| ArtifactRequirement {
                                id: artifact.id.clone(),
                                schema_resource_id: artifact.schema_resource_id.clone(),
                                required: true,
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let effect_requirements = contract
                    .map(|contract| flow_draft_contract_effects(draft, &contract.id))
                    .unwrap_or_default();
                let completion_policy = contract
                    .and_then(|contract| contract.completion.as_ref())
                    .map(CompletionPolicy::from_contract_completion)
                    .unwrap_or_default();
                let stop_gate = match completion_policy {
                    CompletionPolicy::AllArtifacts => StopGate::Required,
                    CompletionPolicy::Manual => StopGate::Preferred,
                    CompletionPolicy::None => StopGate::None,
                };

                Self {
                    node_id: node.id.clone(),
                    contract_id: node.contract_id.clone(),
                    requires,
                    prefers,
                    accepts,
                    completion_policy,
                    artifact_requirements,
                    effect_requirements,
                    stop_gate,
                }
            })
            .collect()
    }
}

impl CompletionPolicy {
    fn from_contract_completion(completion: &ContractCompletion) -> Self {
        match completion {
            ContractCompletion::Manual => Self::Manual,
            ContractCompletion::AllArtifacts => Self::AllArtifacts,
        }
    }
}

impl FlowRepairInput {
    pub fn from_draft(draft: FlowDraft, mode: FlowCheckMode) -> Self {
        Self {
            draft,
            mode,
            route_authoring: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

pub fn flow_repair(input: &FlowRepairInput) -> FlowRepairReport {
    let mut diagnostics = flow_check(&input.draft, input.mode).diagnostics;
    diagnostics.extend(input.diagnostics.clone());

    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity_level == DiagnosticSeverity::Fatal)
    {
        return FlowRepairReport {
            diagnostics,
            patches: Vec::new(),
            candidates: Vec::new(),
        };
    }

    let mut patches = Vec::new();
    let mut candidates = Vec::new();

    for (index, route) in input.route_authoring.iter().enumerate() {
        if route.predicate.is_none() {
            if let Some(when) = non_empty(route.when.as_deref()) {
                patches.push(FlowRepairPatch {
                    repair_kind: RepairKind::RouteWhenToPredicate,
                    location: format!("routes[{}].when", index),
                    replacement: format!("predicate: {when}"),
                });
            }
        }

        match route.predicate.as_ref() {
            Some(RoutePredicateDraft::Artifact(value)) => {
                if let Some(predicate) = artifact_exists_predicate(value) {
                    patches.push(FlowRepairPatch {
                        repair_kind: RepairKind::RouteArtifactObjectToExists,
                        location: format!("routes[{}].predicate.artifact", index),
                        replacement: format!("predicate: {predicate}"),
                    });
                }
            }
            Some(RoutePredicateDraft::Text(value)) => {
                if let Some(predicate) = delivered_artifact_predicate(value) {
                    candidates.push(FlowRepairCandidate {
                        repair_kind: RepairKind::RouteBareArtifactDeliveredToExists,
                        location: format!("routes[{}].predicate", index),
                        replacement: format!("predicate: {predicate}"),
                    });
                }
            }
            None => {}
        }

        if route_activate_missing(route) {
            if let Some(to) = non_empty(route.to.as_deref()) {
                patches.push(FlowRepairPatch {
                    repair_kind: RepairKind::RouteToToActivate,
                    location: format!("routes[{}].to", index),
                    replacement: format!("activate: {to}"),
                });
            } else {
                for node in &input.draft.nodes {
                    candidates.push(FlowRepairCandidate {
                        repair_kind: RepairKind::AddRouteTarget,
                        location: format!("routes[{}].activate", index),
                        replacement: format!("activate: {}", node.id),
                    });
                }
            }
        }
    }

    FlowRepairReport {
        diagnostics,
        patches,
        candidates,
    }
}

pub fn flow_check(draft: &FlowDraft, mode: FlowCheckMode) -> CheckReport {
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
    let mut diagnostics = Vec::new();

    if draft_is_non_empty_package(draft) {
        if !draft_has_readme(draft) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Package,
                "FLOW_MISSING_README",
                "resources",
                "non-empty flow packages must include a README resource",
                "Add a resource with kind 'readme' that describes the package.",
                "Packages need human-readable context before they can be shared or executed safely.",
                DiagnosticRepair::new(Repairability::GuidanceOnly,
                vec![RepairKind::AddReadmeResource]),
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

    for (index, route) in draft.routes.iter().enumerate() {
        if !node_ids.contains(route.activate.as_str()) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Route,
                "FLOW_UNKNOWN_ROUTE_TARGET",
                format!("routes[{}].activate", index),
                format!(
                    "route activate target '{}' does not match a draft node",
                    route.activate
                ),
                "Add the target node or update the route target.",
                "Routes can only activate nodes that exist in the draft.",
                DiagnosticRepair::new(Repairability::Candidate, vec![RepairKind::AddRouteTarget]),
            ));
        }
        if !route_predicate_is_runtime_supported(&route.predicate) {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Route,
                "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
                format!("routes[{}].predicate", index),
                "route predicate must be a runtime-supported fact predicate",
                "Use exists(artifact.key), exists(board.key), or one bare artifact or board fact path.",
                "Route predicates must be executable by preview and activation.",
                DiagnosticRepair::new(Repairability::Candidate,
                vec![RepairKind::RouteBareArtifactDeliveredToExists]),
            ));
        }
        if route
            .for_each
            .as_deref()
            .is_some_and(|expression| !route_for_each_is_artifact_driven(expression))
        {
            diagnostics.push(Diagnostic::error(
                DiagnosticDomain::Route,
                "FLOW_ROUTE_FOR_EACH_NOT_ARTIFACT_DRIVEN",
                format!("routes[{}].for_each", index),
                "route fanout must iterate artifact facts",
                "Use an artifact.* expression for route fanout.",
                "Fanout needs stable artifact data rather than mutable runtime state.",
                DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
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
                DiagnosticRepair::new(
                    Repairability::GuidanceOnly,
                    vec![RepairKind::AddContractCompletion],
                ),
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
                    DiagnosticRepair::new(
                        Repairability::GuidanceOnly,
                        vec![RepairKind::AddArtifactSchema],
                    ),
                )),
                None => diagnostics.push(Diagnostic::error(
                    DiagnosticDomain::Resource,
                    "FLOW_MISSING_ARTIFACT_SCHEMA",
                    format!("contracts[{}].artifacts[{}]", contract.id, artifact.id),
                    format!("artifact '{}' has no schema resource", artifact.id),
                    "Attach the artifact to a schema resource.",
                    "Artifact schemas make delivered data inspectable across nodes.",
                    DiagnosticRepair::new(
                        Repairability::GuidanceOnly,
                        vec![RepairKind::AddArtifactSchema],
                    ),
                )),
            }
        }
    }

    for (index, extension) in draft.extensions.iter().enumerate() {
        if !is_authoring_extension_kind(extension) {
            diagnostics.push(Diagnostic::fatal(
                DiagnosticDomain::Policy,
                "FLOW_AUTHORING_PRIMITIVE_MISUSE",
                format!("extensions[{}]", index),
                format!("'{}' is not a flow authoring primitive", extension),
                "Represent execution details outside FlowDraft authoring data.",
                "Runtime primitives in authoring data can change execution semantics.",
                DiagnosticRepair::new(Repairability::None, Vec::new()),
            ));
        }
    }
    for node in &draft.nodes {
        if let Some(contract_id) = node.contract_id.as_deref() {
            if !contract_ids.contains(contract_id) {
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
        }

        if let Some(action) = &node.action {
            if let Some(prompt_ref) = &action.prompt_ref {
                match resource_by_id.get(prompt_ref.as_str()) {
                    Some(ResourceKind::Prompt) => {}
                    Some(_) => diagnostics.push(Diagnostic::error(
                        DiagnosticDomain::Resource,
                        "FLOW_INVALID_ACTION_PROMPT",
                        format!("nodes[{}].action.prompt_ref", node.id),
                        format!(
                            "node action prompt_ref '{}' must reference a prompt resource",
                            prompt_ref
                        ),
                        "Use a resource with kind 'prompt' for prompt_ref.",
                        "Prompt references need a prompt resource so adapters receive the intended instruction.",
                        DiagnosticRepair::new(Repairability::GuidanceOnly,
                        Vec::new()),
                    )),
                    None => diagnostics.push(Diagnostic::error(
                        DiagnosticDomain::Resource,
                        "FLOW_UNKNOWN_ACTION_PROMPT",
                        format!("nodes[{}].action.prompt_ref", node.id),
                        format!(
                            "node action references missing prompt resource '{}'",
                            prompt_ref
                        ),
                        "Add the prompt resource or update the action prompt reference.",
                        "Prompt references need a resolvable resource before the node can run.",
                        DiagnosticRepair::new(Repairability::GuidanceOnly,
                        Vec::new()),
                    )),
                }
            }

            for (index, resource_id) in action.resource_refs.iter().enumerate() {
                if !resource_by_id.contains_key(resource_id.as_str()) {
                    diagnostics.push(Diagnostic::error(
                        DiagnosticDomain::Resource,
                        "FLOW_UNKNOWN_ACTION_RESOURCE",
                        format!("nodes[{}].action.resource_refs[{}]", node.id, index),
                        format!("node action references missing resource '{}'", resource_id),
                        "Add the resource or update the action resource reference.",
                        "Action resources must exist before adapters can mount or read them.",
                        DiagnosticRepair::new(Repairability::GuidanceOnly, Vec::new()),
                    ));
                }
            }

            for (index, fact_path) in action.reads.iter().enumerate() {
                if !is_fact_path(fact_path) {
                    diagnostics.push(Diagnostic::error(
                        DiagnosticDomain::Contract,
                        "FLOW_INVALID_ACTION_READ",
                        format!("nodes[{}].action.reads[{}]", node.id, index),
                        format!("action read path '{}' is not a fact path", fact_path),
                        "Use artifact.*, board.*, or event.* fact paths.",
                        "Reads define the data contract between runtime state and the node adapter.",
                        DiagnosticRepair::new(Repairability::GuidanceOnly,
                        Vec::new()),
                    ));
                }
            }

            for (index, fact_path) in action.writes.iter().enumerate() {
                if !is_fact_path(fact_path) {
                    diagnostics.push(Diagnostic::error(
                        DiagnosticDomain::Contract,
                        "FLOW_INVALID_ACTION_WRITE",
                        format!("nodes[{}].action.writes[{}]", node.id, index),
                        format!("action write path '{}' is not a fact path", fact_path),
                        "Use artifact.*, board.*, or event.* fact paths.",
                        "Writes define which runtime facts the node may produce.",
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
                    DiagnosticRepair::new(Repairability::GuidanceOnly,
                    Vec::new()),
                ));
            }
        }

        for (index, extension) in node.extensions.iter().enumerate() {
            if !is_authoring_extension_kind(extension) {
                diagnostics.push(Diagnostic::fatal(
                    DiagnosticDomain::Policy,
                    "FLOW_AUTHORING_PRIMITIVE_MISUSE",
                    format!("nodes[{}].extensions[{}]", node.id, index),
                    format!("'{}' is not a flow authoring primitive", extension),
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
            format!("policies.write_scopes[{}]", index),
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

pub fn effective_node_write_scopes(policies: &FlowPolicies, node: &FlowNode) -> Vec<WriteScope> {
    if policies.write_scopes.is_empty() {
        return Vec::new();
    }

    node.write_scopes
        .iter()
        .filter(|scope| policies.write_scopes.contains(scope))
        .cloned()
        .collect()
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

pub fn flow_lock(draft: &FlowDraft, mode: FlowCheckMode) -> Result<FlowLock, FlowLockError> {
    let report = flow_check(draft, mode);
    if report.has_errors() {
        return Err(FlowLockError {
            diagnostics: report.diagnostics,
        });
    }

    let normalized_content = export::normalized_lock_content(draft, mode, &report.diagnostics);
    let id = format!(
        "flk_{:016x}",
        export::stable_hash(normalized_content.as_bytes())
    );

    Ok(FlowLock {
        id,
        mode,
        diagnostics: report.diagnostics,
        draft: draft.clone(),
        normalized_content,
    })
}

pub fn flow_export(lock: &FlowLock, format: FlowExportFormat) -> String {
    export::flow_export(lock, format)
}

impl FlowCheckMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Strict => "strict",
        }
    }
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

impl DiagnosticDomain {
    fn as_str(self) -> &'static str {
        match self {
            Self::Package => "package",
            Self::Contract => "contract",
            Self::Resource => "resource",
            Self::Route => "route",
            Self::Policy => "policy",
            Self::RuntimeCompat => "runtime_compat",
        }
    }
}

impl DiagnosticSeverity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fatal => "fatal",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        }
    }

    fn legacy_severity(self) -> Severity {
        match self {
            Self::Fatal | Self::Error => Severity::Error,
            Self::Warning | Self::Note => Severity::Warning,
        }
    }
}

impl Repairability {
    fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Candidate => "candidate",
            Self::GuidanceOnly => "guidance_only",
            Self::None => "none",
        }
    }
}

impl RepairKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RouteWhenToPredicate => "route_when_to_predicate",
            Self::RouteToToActivate => "route_to_to_activate",
            Self::RouteArtifactObjectToExists => "route_artifact_object_to_exists",
            Self::RouteBareArtifactDeliveredToExists => "route_bare_artifact_delivered_to_exists",
            Self::AddRouteTarget => "add_route_target",
            Self::AddReadmeResource => "add_readme_resource",
            Self::GenerateReadme => "generate_readme",
            Self::AddArtifactSchema => "add_artifact_schema",
            Self::AddContractCompletion => "add_contract_completion",
            Self::NarrowWriteScope => "narrow_write_scope",
            Self::ProvideRuntimeResource => "provide_runtime_resource",
        }
    }
}

impl ContractCompletion {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::AllArtifacts => "all_artifacts",
        }
    }
}

impl NodeDriver {
    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Script => "script",
            Self::Review => "review",
            Self::Human => "human",
        }
    }
}

impl CompletionPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Manual => "manual",
            Self::AllArtifacts => "all_artifacts",
        }
    }
}

impl StopGate {
    fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Preferred => "preferred",
            Self::None => "none",
        }
    }
}

impl ResourceKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Rule => "rule",
            Self::Profile => "profile",
            Self::View => "view",
            Self::Prompt => "prompt",
            Self::Script => "script",
            Self::Flow => "flow",
            Self::Readme => "readme",
        }
    }
}

impl WriteScope {
    fn is_broad(&self) -> bool {
        matches!(self, Self::Workspace | Self::System)
    }

    fn tag(&self) -> &'static str {
        match self {
            Self::Artifact(_) => "artifact",
            Self::Resource(_) => "resource",
            Self::Workspace => "workspace",
            Self::System => "system",
        }
    }

    fn value(&self) -> Option<&str> {
        match self {
            Self::Artifact(value) | Self::Resource(value) => Some(value),
            Self::Workspace | Self::System => None,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct DiagnosticRepair {
    repairability: Repairability,
    repair_kinds: Vec<RepairKind>,
}

impl DiagnosticRepair {
    fn new(repairability: Repairability, repair_kinds: Vec<RepairKind>) -> Self {
        Self {
            repairability,
            repair_kinds,
        }
    }
}

impl Diagnostic {
    fn error(
        domain: DiagnosticDomain,
        code: impl Into<String>,
        location: impl Into<String>,
        message: impl Into<String>,
        fix_hint: impl Into<String>,
        why_it_matters: impl Into<String>,
        repair: DiagnosticRepair,
    ) -> Self {
        Self {
            code: code.into(),
            domain,
            severity: DiagnosticSeverity::Error.legacy_severity(),
            severity_level: DiagnosticSeverity::Error,
            repairability: repair.repairability,
            location: location.into(),
            message: message.into(),
            fix_hint: Some(fix_hint.into()),
            why_it_matters: Some(why_it_matters.into()),
            repair_kinds: repair.repair_kinds,
        }
    }

    fn fatal(
        domain: DiagnosticDomain,
        code: impl Into<String>,
        location: impl Into<String>,
        message: impl Into<String>,
        fix_hint: impl Into<String>,
        why_it_matters: impl Into<String>,
        repair: DiagnosticRepair,
    ) -> Self {
        Self {
            code: code.into(),
            domain,
            severity: DiagnosticSeverity::Fatal.legacy_severity(),
            severity_level: DiagnosticSeverity::Fatal,
            repairability: repair.repairability,
            location: location.into(),
            message: message.into(),
            fix_hint: Some(fix_hint.into()),
            why_it_matters: Some(why_it_matters.into()),
            repair_kinds: repair.repair_kinds,
        }
    }

    fn warning(
        domain: DiagnosticDomain,
        code: impl Into<String>,
        location: impl Into<String>,
        message: impl Into<String>,
        fix_hint: impl Into<String>,
        why_it_matters: impl Into<String>,
        repair: DiagnosticRepair,
    ) -> Self {
        Self {
            code: code.into(),
            domain,
            severity: DiagnosticSeverity::Warning.legacy_severity(),
            severity_level: DiagnosticSeverity::Warning,
            repairability: repair.repairability,
            location: location.into(),
            message: message.into(),
            fix_hint: Some(fix_hint.into()),
            why_it_matters: Some(why_it_matters.into()),
            repair_kinds: repair.repair_kinds,
        }
    }
}

fn push_broad_write_scope_diagnostic(
    scope: &WriteScope,
    severity: Severity,
    location: String,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !scope.is_broad() {
        return;
    }

    let message = format!(
        "write scope '{}' is broader than an artifact or resource",
        scope.tag()
    );
    let fix_hint = "Use artifact or resource write scopes unless wider access is required.";
    let why_it_matters = "Broad write scopes make node effects harder to audit.";
    let diagnostic = match severity {
        Severity::Error => Diagnostic::error(
            DiagnosticDomain::Policy,
            "FLOW_BROAD_WRITE_SCOPE",
            location,
            message,
            fix_hint,
            why_it_matters,
            DiagnosticRepair::new(Repairability::Candidate, vec![RepairKind::NarrowWriteScope]),
        ),
        Severity::Warning => Diagnostic::warning(
            DiagnosticDomain::Policy,
            "FLOW_BROAD_WRITE_SCOPE",
            location,
            message,
            fix_hint,
            why_it_matters,
            DiagnosticRepair::new(Repairability::Candidate, vec![RepairKind::NarrowWriteScope]),
        ),
    };
    diagnostics.push(diagnostic);
}

fn action_contract_fields(action: &NodeAction) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut requires = action.reads.clone();
    let mut prefers = Vec::new();
    if let Some(prompt_ref) = &action.prompt_ref {
        prefers.push(prompt_ref.clone());
    }
    prefers.extend(action.resource_refs.clone());
    let mut accepts = action.writes.clone();
    if let Some(verdict_artifact) = &action.verdict_artifact {
        accepts.push(verdict_artifact.clone());
    }

    dedup_preserving_order(&mut requires);
    dedup_preserving_order(&mut prefers);
    dedup_preserving_order(&mut accepts);

    (requires, prefers, accepts)
}

fn dedup_preserving_order(values: &mut Vec<String>) {
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

fn route_activate_missing(route: &RouteAuthoring) -> bool {
    route
        .activate
        .as_deref()
        .is_none_or(|activate| activate.trim().is_empty())
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn artifact_exists_predicate(value: &str) -> Option<String> {
    artifact_fact_path(value).map(|path| format!("exists({path})"))
}

fn delivered_artifact_predicate(value: &str) -> Option<String> {
    let value = value.trim();
    let artifact = value
        .strip_suffix(".delivered")
        .or_else(|| value.strip_suffix(".ready"))
        .or_else(|| value.strip_suffix(".done"))?;

    if is_fact_path(artifact) && artifact.starts_with("artifact.") {
        Some(format!("exists({artifact})"))
    } else {
        None
    }
}

fn artifact_fact_path(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with("artifact.") {
        return is_fact_path(value).then(|| value.to_string());
    }

    if value.split('.').all(is_fact_path_segment) {
        let candidate = format!("artifact.{value}");
        is_fact_path(&candidate).then_some(candidate)
    } else {
        None
    }
}

fn is_fact_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn route_predicate_is_runtime_supported(predicate: &str) -> bool {
    let trimmed = predicate.trim();

    if let Some(path) = exists_fact_argument(trimmed) {
        return is_executable_route_fact_path(path);
    }

    is_executable_route_fact_path(trimmed)
}

fn is_executable_route_fact_path(path: &str) -> bool {
    is_fact_path(path) && (path.starts_with("artifact.") || path.starts_with("board."))
}

fn exists_fact_argument(predicate: &str) -> Option<&str> {
    predicate
        .strip_prefix("exists(")?
        .strip_suffix(')')
        .map(str::trim)
        .filter(|path| !path.is_empty())
}

fn route_for_each_is_artifact_driven(expression: &str) -> bool {
    let trimmed = expression.trim();
    let cleaned = strip_quoted_strings(trimmed);
    let tokens = identifier_tokens(&cleaned);

    matches!(
        tokens.as_slice(),
        [token] if token.text == trimmed && is_fact_path(token.text) && token.text.starts_with("artifact.")
    )
}

fn is_authoring_extension_kind(extension: &str) -> bool {
    if extension.starts_with(CONTRACT_EFFECTS_EXTENSION_PREFIX) {
        return true;
    }
    matches!(
        extension,
        "Node"
            | "Contract"
            | "Artifact"
            | "Board"
            | "Route"
            | "Event"
            | "Resource"
            | "Import"
            | "Policy"
    )
}

fn draft_is_non_empty_package(draft: &FlowDraft) -> bool {
    !draft.nodes.is_empty()
        || !draft.contracts.is_empty()
        || !draft.routes.is_empty()
        || !draft.resources.is_empty()
        || !draft.imports.is_empty()
        || !draft.policies.write_scopes.is_empty()
        || !draft.extensions.is_empty()
}

fn draft_has_readme(draft: &FlowDraft) -> bool {
    draft
        .resources
        .iter()
        .any(|resource| resource.kind == ResourceKind::Readme)
}

fn draft_has_readme_content(draft: &FlowDraft) -> bool {
    draft
        .resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::Readme)
        .any(|resource| readme_source_has_content(&resource.source))
}

fn readme_source_has_content(source: &str) -> bool {
    let source = source.trim();
    if source.is_empty() {
        return false;
    }

    source
        .strip_prefix("inline:")
        .is_none_or(|inline_source| !inline_source.trim().is_empty())
}

fn unique_ascii_ids(values: &[String], fallback: &str) -> Vec<String> {
    let mut counts = HashMap::new();
    let mut used = HashSet::new();
    values
        .iter()
        .map(|value| {
            let base = slug_ascii_id(value, fallback);
            let mut count = counts.get(&base).copied().unwrap_or(0) + 1;

            loop {
                let candidate = if count == 1 {
                    base.clone()
                } else {
                    format!("{base}_{count}")
                };

                if used.insert(candidate.clone()) {
                    counts.insert(base, count);
                    return candidate;
                }

                count += 1;
            }
        })
        .collect()
}

fn slug_ascii_id(value: &str, fallback: &str) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;

    for character in value.trim().chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_was_separator = false;
        } else if !slug.is_empty() && !last_was_separator {
            slug.push('_');
            last_was_separator = true;
        }
    }

    while slug.ends_with('_') {
        slug.pop();
    }

    if slug.is_empty() {
        fallback.to_string()
    } else {
        slug
    }
}

fn is_fact_path(value: &str) -> bool {
    ["artifact.", "board.", "event."]
        .iter()
        .find_map(|prefix| value.strip_prefix(prefix))
        .is_some_and(|path| {
            path.split('.').all(|segment| {
                !segment.is_empty()
                    && segment
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_')
            })
        })
}

#[derive(Debug, Clone, Copy)]
struct IdentifierToken<'a> {
    text: &'a str,
}

fn identifier_tokens(input: &str) -> Vec<IdentifierToken<'_>> {
    let mut tokens = Vec::new();
    let mut start = None;

    for (index, character) in input.char_indices() {
        if is_identifier_path_char(character) {
            if start.is_none() {
                start = Some(index);
            }
        } else if let Some(token_start) = start.take() {
            push_identifier_token(input, token_start, index, &mut tokens);
        }
    }

    if let Some(token_start) = start {
        push_identifier_token(input, token_start, input.len(), &mut tokens);
    }

    tokens
}

fn push_identifier_token<'a>(
    input: &'a str,
    start: usize,
    end: usize,
    tokens: &mut Vec<IdentifierToken<'a>>,
) {
    let text = input[start..end].trim_matches('.');
    if text.is_empty() || text.chars().all(|character| character.is_ascii_digit()) {
        return;
    }

    tokens.push(IdentifierToken { text });
}

fn is_identifier_path_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character == '.'
}

fn strip_quoted_strings(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(character) = chars.next() {
        if character != '\'' && character != '"' {
            output.push(character);
            continue;
        }

        let quote = character;
        output.push(' ');
        let mut escaped = false;
        for quoted in chars.by_ref() {
            output.push(' ');
            if escaped {
                escaped = false;
                continue;
            }
            if quoted == '\\' {
                escaped = true;
                continue;
            }
            if quoted == quote {
                break;
            }
        }
    }

    output
}
