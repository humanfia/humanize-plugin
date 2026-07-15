#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum CursorPolicy {
    None,
    ExpectedAuthority,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum DriverWire {
    BindRun,
    Context,
    Status,
    Why,
    Pause,
    Resume,
    Complete,
    Stop,
    DeliverArtifact,
    PatchBoard,
    RecordEffect,
    ValidateStop,
    ObserveStop,
    Activate,
    Fanout,
    ApplyFlowRevision,
    PreviewFlowRoutes,
    RecordHookFact,
    SendMessage,
    ViewTerminal,
    ViewSnapshot,
}

impl DriverWire {
    pub(crate) const ALL: &[Self] = &[
        Self::BindRun,
        Self::Context,
        Self::Status,
        Self::Why,
        Self::Pause,
        Self::Resume,
        Self::Complete,
        Self::Stop,
        Self::DeliverArtifact,
        Self::PatchBoard,
        Self::RecordEffect,
        Self::ValidateStop,
        Self::ObserveStop,
        Self::Activate,
        Self::Fanout,
        Self::ApplyFlowRevision,
        Self::PreviewFlowRoutes,
        Self::RecordHookFact,
        Self::SendMessage,
        Self::ViewTerminal,
        Self::ViewSnapshot,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::BindRun => "bind_run",
            Self::Context => "context",
            Self::Status => "status",
            Self::Why => "why",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Complete => "complete",
            Self::Stop => "stop",
            Self::DeliverArtifact => "deliver_artifact",
            Self::PatchBoard => "patch_board",
            Self::RecordEffect => "record_effect",
            Self::ValidateStop => "validate_stop",
            Self::ObserveStop => "observe_stop",
            Self::Activate => "activate",
            Self::Fanout => "fanout",
            Self::ApplyFlowRevision => "apply_flow_revision",
            Self::PreviewFlowRoutes => "preview_flow_routes",
            Self::RecordHookFact => "record_hook_fact",
            Self::SendMessage => "send_message",
            Self::ViewTerminal => "view_terminal",
            Self::ViewSnapshot => "view_snapshot",
        }
    }

    pub(crate) const fn cursor_policy(self) -> CursorPolicy {
        match self {
            Self::BindRun
            | Self::Context
            | Self::Status
            | Self::Why
            | Self::ValidateStop
            | Self::PreviewFlowRoutes
            | Self::ViewTerminal
            | Self::ViewSnapshot => CursorPolicy::None,
            Self::Pause
            | Self::Resume
            | Self::Complete
            | Self::Stop
            | Self::DeliverArtifact
            | Self::PatchBoard
            | Self::RecordEffect
            | Self::ObserveStop
            | Self::Activate
            | Self::Fanout
            | Self::ApplyFlowRevision
            | Self::RecordHookFact
            | Self::SendMessage => CursorPolicy::ExpectedAuthority,
        }
    }

    pub(crate) const fn is_mutation(self) -> bool {
        !matches!(
            self,
            Self::Context
                | Self::Status
                | Self::Why
                | Self::ValidateStop
                | Self::PreviewFlowRoutes
                | Self::ViewTerminal
                | Self::ViewSnapshot
        )
    }
}

pub(crate) fn wire_from_name(name: &str) -> Option<DriverWire> {
    DriverWire::ALL
        .iter()
        .copied()
        .find(|wire| wire.as_str() == name)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn wire_names_are_unique_and_round_trip() {
        let mut names = BTreeSet::new();
        for wire in DriverWire::ALL {
            assert!(names.insert(wire.as_str()));
            assert_eq!(wire_from_name(wire.as_str()), Some(*wire));
        }
    }
}
