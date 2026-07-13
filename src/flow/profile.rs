use serde_json::Value;

use super::{FlowDraft, FlowNode};

const FLOW_QOS_EXTENSION_PREFIX: &str = "humanize.flow_qos:";
const NODE_WORK_PROFILE_EXTENSION_PREFIX: &str = "humanize.work_profile:";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkProfile {
    pub intent: WorkIntent,
    pub workspace_access: WorkspaceAccess,
    pub tool_execution: ToolExecution,
    pub network_access: NetworkAccess,
}

impl Default for WorkProfile {
    fn default() -> Self {
        Self {
            intent: WorkIntent::Produce,
            workspace_access: WorkspaceAccess::ReadWrite,
            tool_execution: ToolExecution::Allowed,
            network_access: NetworkAccess::Restricted,
        }
    }
}

impl WorkProfile {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum WorkIntent {
    Produce,
    Evaluate,
    Explore,
    Synthesize,
    Coordinate,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum WorkspaceAccess {
    None,
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ToolExecution {
    None,
    Allowed,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NetworkAccess {
    None,
    Restricted,
    Open,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FlowQosIntent {
    pub urgency: QosUrgency,
    pub completion_target: Option<String>,
}

impl Default for FlowQosIntent {
    fn default() -> Self {
        Self {
            urgency: QosUrgency::Standard,
            completion_target: None,
        }
    }
}

impl FlowQosIntent {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum QosUrgency {
    Interactive,
    Standard,
    Background,
}

pub fn flow_draft_qos(draft: &FlowDraft) -> FlowQosIntent {
    draft
        .extensions
        .iter()
        .find_map(|extension| parse_qos_extension(extension))
        .unwrap_or_default()
}

pub fn set_flow_draft_qos(draft: &mut FlowDraft, qos: FlowQosIntent) {
    draft
        .extensions
        .retain(|extension| !extension.starts_with(FLOW_QOS_EXTENSION_PREFIX));
    if qos.is_default() {
        return;
    }
    let mut object = serde_json::Map::new();
    object.insert(
        "urgency".to_string(),
        Value::String(qos.urgency.as_str().to_string()),
    );
    if let Some(target) = qos.completion_target {
        object.insert("completion_target".to_string(), Value::String(target));
    }
    draft.extensions.push(format!(
        "{FLOW_QOS_EXTENSION_PREFIX}{}",
        Value::Object(object)
    ));
}

pub fn flow_node_work_profile(node: &FlowNode) -> WorkProfile {
    node.extensions
        .iter()
        .find_map(|extension| parse_work_profile_extension(extension))
        .unwrap_or_default()
}

pub fn set_flow_node_work_profile(node: &mut FlowNode, profile: WorkProfile) {
    node.extensions
        .retain(|extension| !extension.starts_with(NODE_WORK_PROFILE_EXTENSION_PREFIX));
    if profile.is_default() {
        return;
    }
    let object = serde_json::json!({
        "intent": profile.intent.as_str(),
        "workspace_access": profile.workspace_access.as_str(),
        "tool_execution": profile.tool_execution.as_str(),
        "network_access": profile.network_access.as_str(),
    });
    node.extensions
        .push(format!("{NODE_WORK_PROFILE_EXTENSION_PREFIX}{object}"));
}

pub fn extension_is_flow_qos(extension: &str) -> bool {
    extension.starts_with(FLOW_QOS_EXTENSION_PREFIX)
}

pub fn extension_is_node_work_profile(extension: &str) -> bool {
    extension.starts_with(NODE_WORK_PROFILE_EXTENSION_PREFIX)
}

fn parse_qos_extension(extension: &str) -> Option<FlowQosIntent> {
    let payload = extension.strip_prefix(FLOW_QOS_EXTENSION_PREFIX)?;
    let value: Value = serde_json::from_str(payload).ok()?;
    let object = value.as_object()?;
    Some(FlowQosIntent {
        urgency: object
            .get("urgency")
            .and_then(Value::as_str)
            .and_then(QosUrgency::parse)
            .unwrap_or(QosUrgency::Standard),
        completion_target: object
            .get("completion_target")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn parse_work_profile_extension(extension: &str) -> Option<WorkProfile> {
    let payload = extension.strip_prefix(NODE_WORK_PROFILE_EXTENSION_PREFIX)?;
    let value: Value = serde_json::from_str(payload).ok()?;
    let object = value.as_object()?;
    Some(WorkProfile {
        intent: object
            .get("intent")
            .and_then(Value::as_str)
            .and_then(WorkIntent::parse)
            .unwrap_or(WorkIntent::Produce),
        workspace_access: object
            .get("workspace_access")
            .and_then(Value::as_str)
            .and_then(WorkspaceAccess::parse)
            .unwrap_or(WorkspaceAccess::ReadWrite),
        tool_execution: object
            .get("tool_execution")
            .and_then(Value::as_str)
            .and_then(ToolExecution::parse)
            .unwrap_or(ToolExecution::Allowed),
        network_access: object
            .get("network_access")
            .and_then(Value::as_str)
            .and_then(NetworkAccess::parse)
            .unwrap_or(NetworkAccess::Restricted),
    })
}

impl WorkIntent {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Produce => "produce",
            Self::Evaluate => "evaluate",
            Self::Explore => "explore",
            Self::Synthesize => "synthesize",
            Self::Coordinate => "coordinate",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "produce" => Some(Self::Produce),
            "evaluate" => Some(Self::Evaluate),
            "explore" => Some(Self::Explore),
            "synthesize" => Some(Self::Synthesize),
            "coordinate" => Some(Self::Coordinate),
            _ => None,
        }
    }
}

impl WorkspaceAccess {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ReadOnly => "read_only",
            Self::ReadWrite => "read_write",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "read_only" => Some(Self::ReadOnly),
            "read_write" => Some(Self::ReadWrite),
            _ => None,
        }
    }
}

impl ToolExecution {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Allowed => "allowed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "allowed" => Some(Self::Allowed),
            _ => None,
        }
    }
}

impl NetworkAccess {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Restricted => "restricted",
            Self::Open => "open",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "restricted" => Some(Self::Restricted),
            "open" => Some(Self::Open),
            _ => None,
        }
    }
}

impl QosUrgency {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Standard => "standard",
            Self::Background => "background",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "interactive" => Some(Self::Interactive),
            "standard" => Some(Self::Standard),
            "background" => Some(Self::Background),
            _ => None,
        }
    }
}
