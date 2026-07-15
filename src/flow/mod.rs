use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

mod canonical;
mod check;
mod export;
mod package;
mod predicate;
mod profile;
mod suggest;

pub use check::{flow_check, flow_check_run_compatibility, flow_repair};
pub use package::FlowLockDirectoryError;
pub use predicate::{ArtifactRef, FactError, FactKey, FactRef, FlowPredicate};
pub use suggest::flow_suggest;

pub use profile::{
    FlowQosIntent, NetworkAccess, QosUrgency, ToolExecution, WorkIntent, WorkProfile,
    WorkspaceAccess, flow_draft_qos, flow_node_work_profile, set_flow_draft_qos,
    set_flow_node_work_profile,
};
pub(crate) use profile::{extension_is_flow_qos, extension_is_node_work_profile};

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FlowDraft {
    pub nodes: Vec<FlowNode>,
    pub contracts: Vec<FlowContract>,
    pub routes: Vec<FlowRoute>,
    pub resources: Vec<FlowResource>,
    pub imports: Vec<FlowImport>,
    pub policies: FlowPolicies,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FlowNode {
    pub id: String,
    pub contract_id: Option<String>,
    pub action: Option<NodeAction>,
    pub write_scopes: Vec<WriteScope>,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeAction {
    pub driver: NodeDriver,
    pub prompt_ref: Option<String>,
    pub resource_refs: Vec<String>,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub verdict_artifact: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeDriver {
    Agent,
    Script,
    Review,
    Human,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowContract {
    pub id: String,
    pub completion: Option<ContractCompletion>,
    pub artifacts: Vec<ContractArtifact>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractCompletion {
    Manual,
    AllArtifacts,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContractArtifact {
    pub id: String,
    pub schema_resource_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowRoute {
    pub predicate: FlowPredicate,
    pub for_each: Option<ArtifactRef>,
    pub activate: String,
}

pub(crate) fn canonical_route_identity(route: &FlowRoute) -> String {
    let mut hasher = Sha256::new();
    let predicate = route.predicate.to_string();
    let for_each = route
        .for_each
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    for field in [
        predicate.as_str(),
        for_each.as_str(),
        route.activate.as_str(),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    format!("route-sha256:{:x}", hasher.finalize())
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowResource {
    #[serde(rename = "path")]
    pub id: String,
    pub kind: ResourceKind,
    #[serde(rename = "content")]
    pub source: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Schema,
    Rule,
    Profile,
    View,
    Prompt,
    Script,
    Flow,
    Readme,
    Skill,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowImport {
    pub resource_id: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FlowPolicies {
    pub write_scopes: Vec<WriteScope>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdapterCapability {
    pub node_id: String,
    pub driver: NodeDriver,
    pub requires: Vec<String>,
    pub prefers: Vec<String>,
    pub accepts: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionPolicy {
    #[default]
    None,
    Manual,
    AllArtifacts,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ArtifactRequirement {
    pub id: String,
    pub schema_resource_id: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopGate {
    Required,
    Preferred,
    #[default]
    None,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteScope {
    Artifact(String),
    Resource(String),
    Workspace,
    System,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowCheckMode {
    #[default]
    Core,
    Strict,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub domain: DiagnosticDomain,
    pub severity: Severity,
    pub repairability: Repairability,
    pub location: String,
    pub message: String,
    pub fix_hint: Option<String>,
    pub why_it_matters: Option<String>,
    pub repair_kinds: Vec<RepairKind>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticDomain {
    Package,
    Contract,
    Resource,
    Route,
    Policy,
    RuntimeCompat,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Fatal,
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Repairability {
    Automatic,
    Candidate,
    GuidanceOnly,
    None,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairKind {
    AddRouteTarget,
    AddArtifactSchema,
    AddContractCompletion,
    NarrowWriteScope,
    ProvideRuntimeResource,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CheckReport {
    pub mode: FlowCheckMode,
    pub diagnostics: Vec<Diagnostic>,
}

impl CheckReport {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| matches!(diagnostic.severity, Severity::Fatal | Severity::Error))
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunCompatibility {
    pub available_resources: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunCompatibilityResult {
    pub compatible: bool,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowLock {
    id: String,
    content_hash: String,
    mode: FlowCheckMode,
    diagnostics: Vec<Diagnostic>,
    draft: FlowDraft,
    canonical_bytes: Vec<u8>,
}

const FLOW_LOCK_FORMAT: &str = "humanize.flow_lock.v1";

#[derive(Serialize)]
struct FlowLockWireRef<'a> {
    format: &'static str,
    lock_id: &'a str,
    content_hash: &'a str,
    check_mode: FlowCheckMode,
    diagnostics: &'a [Diagnostic],
    flow: &'a FlowDraft,
}

#[derive(Serialize)]
struct FlowLockIdentityRef<'a> {
    format: &'static str,
    check_mode: FlowCheckMode,
    diagnostics: &'a [Diagnostic],
    flow: &'a FlowDraft,
}

#[derive(Deserialize)]
struct FlowLockWire {
    format: String,
    lock_id: String,
    content_hash: String,
    check_mode: FlowCheckMode,
    diagnostics: Vec<Diagnostic>,
    flow: FlowDraft,
}

impl Serialize for FlowLock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        FlowLockWireRef {
            format: FLOW_LOCK_FORMAT,
            lock_id: &self.id,
            content_hash: &self.content_hash,
            check_mode: self.mode,
            diagnostics: &self.diagnostics,
            flow: &self.draft,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FlowLock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FlowLockWire::deserialize(deserializer)?;
        if wire.format != FLOW_LOCK_FORMAT {
            return Err(serde::de::Error::custom("unsupported flow lock format"));
        }
        let lock = flow_lock(&wire.flow, wire.check_mode)
            .map_err(|_| serde::de::Error::custom("flow lock validation failed"))?;
        if wire.flow != lock.draft {
            return Err(serde::de::Error::custom("flow lock flow is not canonical"));
        }
        if wire.lock_id != lock.id || wire.content_hash != lock.content_hash {
            return Err(serde::de::Error::custom(
                "flow lock content identity mismatch",
            ));
        }
        if wire.diagnostics != lock.diagnostics {
            return Err(serde::de::Error::custom("flow lock diagnostics mismatch"));
        }
        Ok(lock)
    }
}

impl FlowLock {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn content_hash(&self) -> &str {
        &self.content_hash
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

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    pub fn write_directory(&self, root: &Path) -> Result<(), FlowLockDirectoryError> {
        package::write_directory(self, root)
    }

    pub fn load_directory(root: &Path) -> Result<Self, FlowLockDirectoryError> {
        package::load_directory(root)
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
    pub diagnostics: Vec<Diagnostic>,
    pub include_warnings: bool,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FlowRepairReport {
    pub diagnostics: Vec<Diagnostic>,
    pub candidates: Vec<FlowRepairCandidate>,
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
    pub readme: String,
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
            diagnostics: Vec::new(),
            include_warnings: false,
        }
    }
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

pub fn flow_lock(draft: &FlowDraft, mode: FlowCheckMode) -> Result<FlowLock, FlowLockError> {
    let report = flow_check(draft, mode);
    if report.has_errors() {
        return Err(FlowLockError {
            diagnostics: report.diagnostics,
        });
    }

    let canonical_draft = canonical::canonicalize_draft(draft);
    let report = flow_check(&canonical_draft, mode);
    let canonical_bytes = serde_json::to_vec(&FlowLockIdentityRef {
        format: FLOW_LOCK_FORMAT,
        check_mode: mode,
        diagnostics: &report.diagnostics,
        flow: &canonical_draft,
    })
    .expect("flow lock identity serialization should not fail");
    let digest = format!("{:x}", Sha256::digest(&canonical_bytes));
    let id = format!("flk_{digest}");
    let content_hash = format!("sha256:{digest}");

    Ok(FlowLock {
        id,
        content_hash,
        mode,
        diagnostics: report.diagnostics,
        draft: canonical_draft,
        canonical_bytes,
    })
}

pub fn flow_export(lock: &FlowLock, format: FlowExportFormat) -> String {
    export::flow_export(lock, format)
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fatal => "fatal",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
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
            Self::Skill => "skill",
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
            severity: Severity::Error,
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
            severity: Severity::Fatal,
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
    let diagnostic = Diagnostic {
        code: "FLOW_BROAD_WRITE_SCOPE".to_string(),
        domain: DiagnosticDomain::Policy,
        severity,
        repairability: Repairability::GuidanceOnly,
        location,
        message,
        fix_hint: Some(fix_hint.to_string()),
        why_it_matters: Some(why_it_matters.to_string()),
        repair_kinds: Vec::new(),
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

fn is_authoring_extension_kind(extension: &str) -> bool {
    if extension.starts_with(CONTRACT_EFFECTS_EXTENSION_PREFIX)
        || extension_is_flow_qos(extension)
        || extension_is_node_work_profile(extension)
    {
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
