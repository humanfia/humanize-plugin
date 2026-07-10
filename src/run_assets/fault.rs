use std::cell::Cell;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RunAssetFaultPoint {
    RegisterExpectedActivation,
    StartActivationTranscript,
    StartActivationMetadata,
    StartActivationManifest,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetFault {
    pub(super) point: RunAssetFaultKind,
    pub(super) activation_id: Option<String>,
    pub(super) trigger_once: bool,
    pub(super) triggered: Cell<bool>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum RunAssetFaultKind {
    RegisterExpectedActivation,
    StartActivationTranscript,
    StartActivationMetadata,
    StartActivationManifest,
    ResourceCleanupStatus,
}

impl RunAssetFaultKind {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::RegisterExpectedActivation => "register_expected_activation",
            Self::StartActivationTranscript => "start_activation_transcript",
            Self::StartActivationMetadata => "start_activation_metadata",
            Self::StartActivationManifest => "start_activation_manifest",
            Self::ResourceCleanupStatus => "resource_cleanup_status",
        }
    }
}

impl From<RunAssetFaultPoint> for RunAssetFaultKind {
    fn from(point: RunAssetFaultPoint) -> Self {
        match point {
            RunAssetFaultPoint::RegisterExpectedActivation => Self::RegisterExpectedActivation,
            RunAssetFaultPoint::StartActivationTranscript => Self::StartActivationTranscript,
            RunAssetFaultPoint::StartActivationMetadata => Self::StartActivationMetadata,
            RunAssetFaultPoint::StartActivationManifest => Self::StartActivationManifest,
        }
    }
}

impl RunAssetFaultPoint {
    pub fn for_activation(self, activation_id: impl Into<String>) -> RunAssetFault {
        RunAssetFault {
            point: self.into(),
            activation_id: Some(activation_id.into()),
            trigger_once: false,
            triggered: Cell::new(false),
        }
    }
}

impl RunAssetFault {
    pub(super) fn resource_cleanup_once(activation_id: impl Into<String>) -> Self {
        Self {
            point: RunAssetFaultKind::ResourceCleanupStatus,
            activation_id: Some(activation_id.into()),
            trigger_once: true,
            triggered: Cell::new(false),
        }
    }
}
