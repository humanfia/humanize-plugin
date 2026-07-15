use std::io;
use std::path::{Path, PathBuf};

use crate::participant_binding::ParticipantBindingFile;

use super::registry::CallerKind;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum McpCaller {
    Operator,
    Participant(ParticipantCaller),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct ParticipantCaller {
    pub(super) run_id: String,
    pub(super) activation_id: String,
    pub(super) handle: String,
    pub(super) credential: String,
    pub(super) runs_root: PathBuf,
}

impl McpCaller {
    pub(super) fn from_environment() -> io::Result<Self> {
        let Some((_, binding)) = ParticipantBindingFile::from_environment()? else {
            return Ok(Self::Operator);
        };
        Ok(Self::Participant(ParticipantCaller {
            run_id: binding.run_id,
            activation_id: binding.activation_id,
            handle: binding.handle,
            credential: binding.credential,
            runs_root: binding.runs_root,
        }))
    }

    pub(super) const fn kind(&self) -> CallerKind {
        match self {
            Self::Operator => CallerKind::Operator,
            Self::Participant(_) => CallerKind::Participant,
        }
    }

    pub(super) fn runs_root(&self) -> Option<&Path> {
        match self {
            Self::Operator => None,
            Self::Participant(participant) => Some(&participant.runs_root),
        }
    }
}
