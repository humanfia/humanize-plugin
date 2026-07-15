use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::RunAssetTmuxTarget;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RunAssetSink {
    Auto,
    HumanizeRunsDir(PathBuf),
    #[deprecated(note = "use HumanizeRunsDir; runtime_default does not inspect SFORGE_PATCH_DIR")]
    SforgePatchDir(PathBuf),
    CacheHome(PathBuf),
    Root(PathBuf),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum RunAssetClock {
    Realtime,
    Fixed(u64),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum SelectedRunAssetSink {
    HumanizeRunsDir(PathBuf),
    CacheHome(PathBuf),
    Root(PathBuf),
}

impl SelectedRunAssetSink {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::HumanizeRunsDir(_) => "humanize_runs_dir",
            Self::CacheHome(_) => "cache_home",
            Self::Root(_) => "root",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetManifest {
    pub version: u32,
    pub run_id: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub sink: String,
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub storage: RunAssetStorage,
    pub protocol: RunAssetProtocol,
    pub flow: RunAssetFlow,
    pub artifact_paths: RunAssetArtifactPaths,
    pub activations: BTreeMap<String, RunAssetActivation>,
    pub preservation_errors: Vec<RunAssetPreservationError>,
    pub preservation_blocked: bool,
    pub completion: RunAssetCompletion,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetStorage {
    pub raw_run_id: String,
    pub run_directory: String,
    pub run_relative_path: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetProtocol {
    pub mcp_protocol_version: String,
    pub package_name: String,
    pub package_version: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetFlow {
    pub main_flow: bool,
    pub status: String,
    pub complete: bool,
    pub current_revision_id: Option<String>,
    pub current_export_path: Option<PathBuf>,
    pub current_export_relative_path: Option<String>,
    pub revisions: Vec<RunAssetFlowRevision>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetFlowRevision {
    pub revision_id: String,
    pub main_flow: bool,
    pub flow_lock_id: String,
    pub content_hash: String,
    pub review_status: String,
    pub flow_lock_mode: String,
    pub export_format: String,
    pub export_path: PathBuf,
    pub relative_path: String,
    pub created_at_ms: u64,
    pub apply_state: String,
    pub applied_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetArtifactPaths {
    pub manifest: PathBuf,
    pub manifest_relative_path: String,
    pub flow_current: Option<PathBuf>,
    pub flow_current_relative_path: Option<String>,
    pub flow_revisions: Vec<PathBuf>,
    pub flow_revision_relative_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetCompletion {
    pub flow_complete: bool,
    pub expected_tmux_activations: Vec<String>,
    pub complete_tmux_activations: Vec<String>,
    pub incomplete_tmux_activations: Vec<String>,
    pub complete: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetActivation {
    pub run_id: String,
    pub activation_id: String,
    pub node_id: String,
    pub adapter: String,
    #[serde(default)]
    pub tmux_target: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub window_id: String,
    #[serde(default)]
    pub window_name: String,
    #[serde(default)]
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tmux_target_ref: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session_ref: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub window_ref: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub window_name_ref: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pane_ref: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub allocation_generation: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub readiness_nonce: String,
    pub metadata_path: PathBuf,
    pub pipe_path: PathBuf,
    pub final_capture_path: PathBuf,
    pub relative_paths: RunAssetActivationRelativePaths,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub termination_reason: Option<String>,
    pub capture_phase: String,
    pub pipe_acknowledged: bool,
    pub capture_complete: bool,
    pub preservation_status: String,
    pub resource_cleanup_status: String,
    pub resource_cleanup_error: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetActivationRelativePaths {
    pub metadata: String,
    pub transcript_pipe: String,
    pub final_capture: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetActivationUpdate {
    pub activation_id: String,
    pub node_id: String,
    pub tmux: RunAssetTmuxTarget,
    pub adapter: String,
    pub termination_reason: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetActivationFailureUpdate {
    pub activation_id: String,
    pub node_id: String,
    pub tmux: Option<RunAssetTmuxTarget>,
    pub adapter: String,
    pub termination_reason: Option<String>,
    pub stage: String,
    pub error: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAssetPreservationError {
    pub activation_id: Option<String>,
    pub stage: String,
    pub error: String,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunAssetError {
    message: String,
    publication_blocked: bool,
}

impl RunAssetError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            publication_blocked: false,
        }
    }

    pub(crate) fn publication(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            publication_blocked: true,
        }
    }

    pub(crate) fn publication_from(error: Self) -> Self {
        Self::publication(error.message)
    }

    pub(crate) fn is_publication_blocked(&self) -> bool {
        self.publication_blocked
    }
}

impl fmt::Display for RunAssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl Error for RunAssetError {}

fn is_zero(value: &u64) -> bool {
    *value == 0
}
