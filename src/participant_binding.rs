use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::run_assets::{atomic_write_private, read_regular_private};

pub(crate) const PARTICIPANT_BINDING_FILE_ENV: &str = "HUMANIZE_PARTICIPANT_BINDING_FILE";
const PARTICIPANT_BINDING_PROTOCOL: &str = "humanize.participant_binding.v1";

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ParticipantBindingFile {
    protocol: String,
    pub(crate) run_id: String,
    pub(crate) activation_id: String,
    pub(crate) allocation_generation: u64,
    pub(crate) pane_id: String,
    pub(crate) readiness_nonce: String,
    pub(crate) handle: String,
    pub(crate) credential: String,
    pub(crate) runs_root: PathBuf,
}

impl ParticipantBindingFile {
    pub(crate) fn new(
        run_id: String,
        activation_id: String,
        allocation_generation: u64,
        pane_id: String,
        readiness_nonce: String,
        handle: String,
        credential: String,
    ) -> Self {
        Self {
            protocol: PARTICIPANT_BINDING_PROTOCOL.to_string(),
            run_id,
            activation_id,
            allocation_generation,
            pane_id,
            readiness_nonce,
            handle,
            credential,
            runs_root: PathBuf::new(),
        }
    }

    pub(crate) fn with_runs_root(mut self, runs_root: PathBuf) -> Self {
        self.runs_root = runs_root;
        self
    }

    pub(crate) fn write(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        bytes.push(b'\n');
        atomic_write_private(path, &bytes).map_err(|err| io::Error::other(err.to_string()))
    }

    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        let bytes = read_regular_private(path)
            .map_err(|err| io::Error::other(err.to_string()))?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "participant binding file is missing",
                )
            })?;
        let binding = serde_json::from_slice::<Self>(&bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        binding.validate()?;
        Ok(binding)
    }

    pub(crate) fn from_environment() -> io::Result<Option<(PathBuf, Self)>> {
        let Some(path) = std::env::var_os(PARTICIPANT_BINDING_FILE_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
        else {
            return Ok(None);
        };
        let binding = Self::read(&path)?;
        Ok(Some((path, binding)))
    }

    fn validate(&self) -> io::Result<()> {
        if self.protocol != PARTICIPANT_BINDING_PROTOCOL {
            return Err(invalid_data("participant binding protocol mismatch"));
        }
        if [
            self.run_id.as_str(),
            self.activation_id.as_str(),
            self.pane_id.as_str(),
            self.readiness_nonce.as_str(),
            self.handle.as_str(),
            self.credential.as_str(),
        ]
        .into_iter()
        .any(|value| value.trim().is_empty())
        {
            return Err(invalid_data("participant binding fields must be non-empty"));
        }
        if self.runs_root.as_os_str().is_empty() {
            return Err(invalid_data("participant runs root must be non-empty"));
        }
        Ok(())
    }
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
