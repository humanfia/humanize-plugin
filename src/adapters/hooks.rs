use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct StopHookInput {
    session_id: String,
    activation_id: String,
}

impl StopHookInput {
    pub fn new(session_id: impl Into<String>, activation_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            activation_id: activation_id.into(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn activation_id(&self) -> &str {
        &self.activation_id
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DriverDecision {
    Allow,
    Block { reason: String },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookAction {
    Allow,
    Block,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct NeutralHookPayload {
    action: HookAction,
    session_id: String,
    activation_id: String,
    reason: Option<String>,
}

impl NeutralHookPayload {
    pub fn new(
        action: HookAction,
        session_id: impl Into<String>,
        activation_id: impl Into<String>,
        reason: Option<String>,
    ) -> Self {
        Self {
            action,
            session_id: session_id.into(),
            activation_id: activation_id.into(),
            reason,
        }
    }

    pub fn action(&self) -> HookAction {
        self.action
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn activation_id(&self) -> &str {
        &self.activation_id
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

pub fn build_stop_hook_payload(
    input: &StopHookInput,
    decision: DriverDecision,
) -> NeutralHookPayload {
    match decision {
        DriverDecision::Allow => NeutralHookPayload::new(
            HookAction::Allow,
            input.session_id(),
            input.activation_id(),
            None,
        ),
        DriverDecision::Block { reason } => NeutralHookPayload::new(
            HookAction::Block,
            input.session_id(),
            input.activation_id(),
            Some(reason),
        ),
    }
}
