use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct StopObservation {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_id: Option<String>,
}

impl StopObservation {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            invocation_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopDecisionKind {
    Allow,
    Deny,
    Block,
    Yield,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct StopDecision {
    pub kind: StopDecisionKind,
    pub attempt: u32,
    pub missing_artifacts: Vec<String>,
    pub missing_effects: Vec<String>,
    pub reason: Option<String>,
}

impl StopDecision {
    pub fn allow(attempt: u32) -> Self {
        Self {
            kind: StopDecisionKind::Allow,
            attempt,
            missing_artifacts: Vec::new(),
            missing_effects: Vec::new(),
            reason: None,
        }
    }

    pub fn deny_until_limit(
        attempt: u32,
        missing_artifacts: Vec<String>,
        missing_effects: Vec<String>,
    ) -> Self {
        Self {
            kind: StopDecisionKind::Deny,
            attempt,
            missing_artifacts,
            missing_effects,
            reason: Some("missing stop requirements".into()),
        }
    }

    pub fn block(
        attempt: u32,
        missing_artifacts: Vec<String>,
        missing_effects: Vec<String>,
    ) -> Self {
        Self {
            kind: StopDecisionKind::Block,
            attempt,
            missing_artifacts,
            missing_effects,
            reason: Some("stop validation limit reached".into()),
        }
    }

    pub fn yield_now(attempt: u32, reason: impl Into<String>) -> Self {
        Self {
            kind: StopDecisionKind::Yield,
            attempt,
            missing_artifacts: Vec::new(),
            missing_effects: Vec::new(),
            reason: Some(reason.into()),
        }
    }
}
