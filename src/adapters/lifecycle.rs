#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct AdapterCapabilities {
    pub interactive_pane: bool,
    pub mcp_tools: bool,
    pub mcp_artifact_delivery: bool,
    pub stop_hook: bool,
    pub tool_events: bool,
    pub permission_events: bool,
    pub notification_events: bool,
    pub jsonl_events: bool,
    pub session_resume: bool,
    pub process_exit: bool,
    pub tmux_observation: bool,
    pub structured_output: bool,
}

impl AdapterCapabilities {
    pub const fn tmux_lifecycle() -> Self {
        Self {
            interactive_pane: true,
            mcp_tools: false,
            mcp_artifact_delivery: false,
            stop_hook: false,
            tool_events: false,
            permission_events: false,
            notification_events: false,
            jsonl_events: false,
            session_resume: false,
            process_exit: true,
            tmux_observation: true,
            structured_output: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LifecycleStatus {
    Running,
    ContractSatisfied,
    Blocked,
    Failed,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LifecycleCleanupAction {
    KillPane,
    PreservePane,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct LifecycleCleanup {
    action: LifecycleCleanupAction,
    status: LifecycleStatus,
}

impl LifecycleCleanup {
    pub fn new(action: LifecycleCleanupAction, status: LifecycleStatus) -> Self {
        Self { action, status }
    }

    pub fn action(&self) -> LifecycleCleanupAction {
        self.action
    }

    pub fn status(&self) -> LifecycleStatus {
        self.status
    }
}

pub trait AgentLifecycleAdapter {
    type ActivationRequest;
    type Activation;
    type Handle;
    type Observation;
    type Error;

    fn capabilities(&self) -> AdapterCapabilities;

    fn prepare_activation(
        &self,
        request: Self::ActivationRequest,
    ) -> Result<Self::Activation, Self::Error>;

    fn start_agent(
        &self,
        activation: &Self::Activation,
        command: &str,
    ) -> Result<Self::Handle, Self::Error>;

    fn send_prompt(&self, handle: &Self::Handle, prompt: &str) -> Result<(), Self::Error>;

    fn observe_lifecycle(&self, handle: &Self::Handle) -> Result<Self::Observation, Self::Error>;

    fn cleanup_activation(
        &self,
        handle: &Self::Handle,
        status: LifecycleStatus,
    ) -> Result<LifecycleCleanup, Self::Error>;
}
