use std::collections::{HashMap, HashSet};

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
    pub severity: Severity,
    pub location: String,
    pub message: String,
    pub fix_hint: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct CheckReport {
    pub mode: FlowCheckMode,
    pub diagnostics: Vec<Diagnostic>,
}

impl CheckReport {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
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

    pub fn normalized_content(&self) -> &str {
        &self.normalized_content
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowLockError {
    pub diagnostics: Vec<Diagnostic>,
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

pub fn flow_check(draft: &FlowDraft, mode: FlowCheckMode) -> CheckReport {
    let node_ids = draft
        .nodes
        .iter()
        .map(|node| node.id.as_str())
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
                "FLOW_MISSING_README",
                "resources",
                "non-empty flow packages must include a README resource",
                "Add a resource with kind 'readme' that describes the package.",
            ));
        } else if !draft_has_readme_content(draft) {
            diagnostics.push(Diagnostic::error(
                "FLOW_EMPTY_README",
                "resources",
                "README resources must include explanatory content",
                "Add non-empty content to at least one README resource.",
            ));
        }
    }

    for (index, route) in draft.routes.iter().enumerate() {
        if !node_ids.contains(route.activate.as_str()) {
            diagnostics.push(Diagnostic::error(
                "FLOW_UNKNOWN_ROUTE_TARGET",
                format!("routes[{}].activate", index),
                format!(
                    "route activate target '{}' does not match a draft node",
                    route.activate
                ),
                "Add the target node or update the route target.",
            ));
        }
        if !route_predicate_is_fact_driven(&route.predicate) {
            diagnostics.push(Diagnostic::error(
                "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
                format!("routes[{}].predicate", index),
                "route predicate must read only artifact.*, board.*, event.* facts or exists(...)",
                "Move effects into runtime tools and keep the route predicate fact-driven.",
            ));
        }
        if route
            .for_each
            .as_deref()
            .is_some_and(|expression| !route_for_each_is_artifact_driven(expression))
        {
            diagnostics.push(Diagnostic::error(
                "FLOW_ROUTE_FOR_EACH_NOT_ARTIFACT_DRIVEN",
                format!("routes[{}].for_each", index),
                "route fanout must iterate artifact facts",
                "Use an artifact.* expression for route fanout.",
            ));
        }
    }

    for contract in &draft.contracts {
        if contract.completion.is_none() {
            diagnostics.push(Diagnostic::error(
                "FLOW_MISSING_CONTRACT_COMPLETION",
                format!("contracts[{}].completion", contract.id),
                format!("contract '{}' has no completion rule", contract.id),
                "Set a completion rule such as Manual or AllArtifacts.",
            ));
        }

        for artifact in &contract.artifacts {
            match artifact.schema_resource_id.as_deref() {
                Some(schema_id) if schema_ids.contains(schema_id) => {}
                Some(schema_id) => diagnostics.push(Diagnostic::error(
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
                )),
                None => diagnostics.push(Diagnostic::error(
                    "FLOW_MISSING_ARTIFACT_SCHEMA",
                    format!("contracts[{}].artifacts[{}]", contract.id, artifact.id),
                    format!("artifact '{}' has no schema resource", artifact.id),
                    "Attach the artifact to a schema resource.",
                )),
            }
        }
    }

    for (index, extension) in draft.extensions.iter().enumerate() {
        if !is_authoring_extension_kind(extension) {
            diagnostics.push(Diagnostic::error(
                "FLOW_AUTHORING_PRIMITIVE_MISUSE",
                format!("extensions[{}]", index),
                format!("'{}' is not a flow authoring primitive", extension),
                "Represent execution details outside FlowDraft authoring data.",
            ));
        }
    }
    for node in &draft.nodes {
        if let Some(action) = &node.action {
            if let Some(prompt_ref) = &action.prompt_ref {
                match resource_by_id.get(prompt_ref.as_str()) {
                    Some(ResourceKind::Prompt) => {}
                    Some(_) => diagnostics.push(Diagnostic::error(
                        "FLOW_INVALID_ACTION_PROMPT",
                        format!("nodes[{}].action.prompt_ref", node.id),
                        format!(
                            "node action prompt_ref '{}' must reference a prompt resource",
                            prompt_ref
                        ),
                        "Use a resource with kind 'prompt' for prompt_ref.",
                    )),
                    None => diagnostics.push(Diagnostic::error(
                        "FLOW_UNKNOWN_ACTION_PROMPT",
                        format!("nodes[{}].action.prompt_ref", node.id),
                        format!(
                            "node action references missing prompt resource '{}'",
                            prompt_ref
                        ),
                        "Add the prompt resource or update the action prompt reference.",
                    )),
                }
            }

            for (index, resource_id) in action.resource_refs.iter().enumerate() {
                if !resource_by_id.contains_key(resource_id.as_str()) {
                    diagnostics.push(Diagnostic::error(
                        "FLOW_UNKNOWN_ACTION_RESOURCE",
                        format!("nodes[{}].action.resource_refs[{}]", node.id, index),
                        format!("node action references missing resource '{}'", resource_id),
                        "Add the resource or update the action resource reference.",
                    ));
                }
            }

            for (index, fact_path) in action.reads.iter().enumerate() {
                if !is_fact_path(fact_path) {
                    diagnostics.push(Diagnostic::error(
                        "FLOW_INVALID_ACTION_READ",
                        format!("nodes[{}].action.reads[{}]", node.id, index),
                        format!("action read path '{}' is not a fact path", fact_path),
                        "Use artifact.*, board.*, or event.* fact paths.",
                    ));
                }
            }

            for (index, fact_path) in action.writes.iter().enumerate() {
                if !is_fact_path(fact_path) {
                    diagnostics.push(Diagnostic::error(
                        "FLOW_INVALID_ACTION_WRITE",
                        format!("nodes[{}].action.writes[{}]", node.id, index),
                        format!("action write path '{}' is not a fact path", fact_path),
                        "Use artifact.*, board.*, or event.* fact paths.",
                    ));
                }
            }

            if action
                .verdict_artifact
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                diagnostics.push(Diagnostic::error(
                    "FLOW_EMPTY_ACTION_VERDICT_ARTIFACT",
                    format!("nodes[{}].action.verdict_artifact", node.id),
                    "action verdict artifact must not be empty",
                    "Use a non-empty artifact-like id such as artifact.verdict.",
                ));
            }
        }

        for (index, extension) in node.extensions.iter().enumerate() {
            if !is_authoring_extension_kind(extension) {
                diagnostics.push(Diagnostic::error(
                    "FLOW_AUTHORING_PRIMITIVE_MISUSE",
                    format!("nodes[{}].extensions[{}]", node.id, index),
                    format!("'{}' is not a flow authoring primitive", extension),
                    "Represent execution details outside FlowDraft authoring data.",
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
                "FLOW_RUN_RESOURCE_UNAVAILABLE",
                format!("resources[{}]", resource.id),
                format!("resource '{}' is not available to the run", resource.id),
                "Provide the resource to the run or remove it from the draft.",
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

    let normalized_draft = normalize_draft(draft);
    let normalized_diagnostics = normalize_diagnostics(&report.diagnostics);
    let normalized_content = format!(
        "{{\"mode\":{},\"draft\":{},\"diagnostics\":{}}}",
        quote(mode.as_str()),
        normalized_draft,
        normalized_diagnostics
    );
    let id = format!("flk_{:016x}", stable_hash(normalized_content.as_bytes()));

    Ok(FlowLock {
        id,
        mode,
        diagnostics: report.diagnostics,
        normalized_content,
    })
}

pub fn flow_export(lock: &FlowLock, format: FlowExportFormat) -> String {
    match format {
        FlowExportFormat::Json => export_lock_json(lock),
        FlowExportFormat::Yaml => export_lock_yaml(lock),
    }
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

impl Diagnostic {
    fn error(
        code: impl Into<String>,
        location: impl Into<String>,
        message: impl Into<String>,
        fix_hint: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity: Severity::Error,
            location: location.into(),
            message: message.into(),
            fix_hint: Some(fix_hint.into()),
        }
    }

    fn warning(
        code: impl Into<String>,
        location: impl Into<String>,
        message: impl Into<String>,
        fix_hint: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity: Severity::Warning,
            location: location.into(),
            message: message.into(),
            fix_hint: Some(fix_hint.into()),
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
    let diagnostic = match severity {
        Severity::Error => Diagnostic::error("FLOW_BROAD_WRITE_SCOPE", location, message, fix_hint),
        Severity::Warning => {
            Diagnostic::warning("FLOW_BROAD_WRITE_SCOPE", location, message, fix_hint)
        }
    };
    diagnostics.push(diagnostic);
}

fn route_predicate_is_fact_driven(predicate: &str) -> bool {
    let cleaned = strip_quoted_strings(predicate);
    let tokens = identifier_tokens(&cleaned);

    if tokens.is_empty() {
        return false;
    }

    let mut has_fact_path = false;
    for token in tokens {
        let ident = token.text;
        if is_boolean_literal(ident) {
            continue;
        }
        if ident == "exists" {
            if next_non_whitespace(&cleaned, token.end) != Some('(') {
                return false;
            }
            continue;
        }
        if next_non_whitespace(&cleaned, token.end) == Some('(') {
            return false;
        }
        if !is_fact_path(ident) {
            return false;
        }
        has_fact_path = true;
    }

    has_fact_path
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

fn is_boolean_literal(value: &str) -> bool {
    matches!(value, "true" | "false")
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
    end: usize,
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

    let leading_dots = input[start..end].len() - input[start..end].trim_start_matches('.').len();
    let adjusted_start = start + leading_dots;
    let adjusted_end = adjusted_start + text.len();

    tokens.push(IdentifierToken {
        text,
        end: adjusted_end,
    });
}

fn is_identifier_path_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character == '.'
}

fn next_non_whitespace(input: &str, start: usize) -> Option<char> {
    input[start..]
        .chars()
        .find(|character| !character.is_ascii_whitespace())
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
            .then(left.location.cmp(&right.location))
            .then(left.message.cmp(&right.message))
            .then(left.fix_hint.cmp(&right.fix_hint))
    });

    let values = sorted.iter().map(normalize_diagnostic).collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn normalize_diagnostic(diagnostic: &Diagnostic) -> String {
    format!(
        "{{\"code\":{},\"severity\":{},\"location\":{},\"message\":{},\"fix_hint\":{}}}",
        quote(&diagnostic.code),
        quote(diagnostic.severity.as_str()),
        quote(&diagnostic.location),
        quote(&diagnostic.message),
        quote_option(diagnostic.fix_hint.as_deref())
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
                format!(
                    "  - code: {}\n    severity: {}\n    location: {}\n    message: {}\n    fix_hint: {}",
                    yaml_scalar(&diagnostic.code),
                    yaml_scalar(diagnostic.severity.as_str()),
                    yaml_scalar(&diagnostic.location),
                    yaml_scalar(&diagnostic.message),
                    diagnostic
                        .fix_hint
                        .as_deref()
                        .map(yaml_scalar)
                        .unwrap_or_else(|| "null".into())
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

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
