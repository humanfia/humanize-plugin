use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::adapters::tmux::{
    CommandRunner, TmuxAdapter, TmuxError, TmuxPane, TmuxSession, TmuxWindow,
};

pub const REAL_TEST_SESSION_ID: &str = "humanize-plugin-real-test";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ToolKind {
    Codex,
    Claude,
}

impl ToolKind {
    pub fn slug(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    pub fn display(self) -> &'static str {
        self.slug()
    }
}

impl fmt::Display for ToolKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.slug())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DataPoint {
    flow_slug: String,
    project_slug: String,
    tool_kind: ToolKind,
    workdir: PathBuf,
}

impl DataPoint {
    pub fn new(
        flow_slug: impl Into<String>,
        project_slug: impl Into<String>,
        tool_kind: ToolKind,
        workdir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            flow_slug: flow_slug.into(),
            project_slug: project_slug.into(),
            tool_kind,
            workdir: workdir.into(),
        }
    }

    pub fn flow_slug(&self) -> &str {
        &self.flow_slug
    }

    pub fn project_slug(&self) -> &str {
        &self.project_slug
    }

    pub fn tool_kind(&self) -> ToolKind {
        self.tool_kind
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RealTestTopology {
    session_id: String,
    flow_slug: String,
    project_slug: String,
    tool_kind: ToolKind,
    workdir: PathBuf,
    window_name: String,
    pane_label: String,
    identity: String,
}

impl RealTestTopology {
    pub fn new(
        session_id: impl Into<String>,
        data_point: &DataPoint,
    ) -> Result<Self, RealTestError> {
        let session_id = session_id.into();
        validate_real_test_session_id(&session_id)?;
        let window_name = data_point.flow_slug().to_string();
        let pane_label = format!(
            "{}-{}",
            data_point.project_slug(),
            data_point.tool_kind().slug()
        );
        let identity = format!("{session_id}:{window_name}.{pane_label}");

        Ok(Self {
            session_id,
            flow_slug: data_point.flow_slug().to_string(),
            project_slug: data_point.project_slug().to_string(),
            tool_kind: data_point.tool_kind(),
            workdir: data_point.workdir().to_path_buf(),
            window_name,
            pane_label,
            identity,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn flow_slug(&self) -> &str {
        &self.flow_slug
    }

    pub fn project_slug(&self) -> &str {
        &self.project_slug
    }

    pub fn tool_kind(&self) -> ToolKind {
        self.tool_kind
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    pub fn window_name(&self) -> &str {
        &self.window_name
    }

    pub fn pane_label(&self) -> &str {
        &self.pane_label
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    fn lease(
        &self,
        window_id: impl Into<String>,
        pane_id: impl Into<String>,
        generation: u64,
    ) -> RealTestLease {
        RealTestLease {
            session_id: self.session_id.clone(),
            window_id: window_id.into(),
            window_name: self.window_name.clone(),
            pane_id: pane_id.into(),
            flow_slug: self.flow_slug.clone(),
            project_slug: self.project_slug.clone(),
            tool_kind: self.tool_kind,
            workdir: self.workdir.clone(),
            pane_label: self.pane_label.clone(),
            generation,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RealTestLease {
    session_id: String,
    window_id: String,
    window_name: String,
    pane_id: String,
    flow_slug: String,
    project_slug: String,
    tool_kind: ToolKind,
    workdir: PathBuf,
    pane_label: String,
    generation: u64,
}

impl RealTestLease {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn window_id(&self) -> &str {
        &self.window_id
    }

    pub fn window_name(&self) -> &str {
        &self.window_name
    }

    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    pub fn flow_slug(&self) -> &str {
        &self.flow_slug
    }

    pub fn project_slug(&self) -> &str {
        &self.project_slug
    }

    pub fn tool_kind(&self) -> ToolKind {
        self.tool_kind
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    pub fn pane_label(&self) -> &str {
        &self.pane_label
    }

    fn identity(&self) -> String {
        format!(
            "{}:{}.{}",
            self.session_id, self.window_name, self.pane_label
        )
    }

    fn tmux_pane(&self) -> TmuxPane {
        TmuxPane::new_in_session(
            self.session_id.as_str(),
            self.window_id.as_str(),
            self.pane_label.as_str(),
            self.pane_id.as_str(),
        )
    }

    fn tmux_window(&self) -> TmuxWindow {
        TmuxWindow::new_named(
            self.session_id.as_str(),
            self.flow_slug.as_str(),
            self.window_name.as_str(),
            self.window_id.as_str(),
        )
    }
}

#[derive(Debug)]
pub struct RealTestAllocator<R: CommandRunner> {
    adapter: TmuxAdapter<R>,
    session_id: String,
    generation: u64,
    session: Option<TmuxSession>,
    windows: BTreeMap<String, TmuxWindow>,
    panes: BTreeMap<String, RealTestLease>,
    identity_index: BTreeMap<String, String>,
}

impl<R: CommandRunner> RealTestAllocator<R> {
    pub fn new(adapter: TmuxAdapter<R>) -> Self {
        Self {
            adapter,
            session_id: REAL_TEST_SESSION_ID.to_string(),
            generation: 0,
            session: None,
            windows: BTreeMap::new(),
            panes: BTreeMap::new(),
            identity_index: BTreeMap::new(),
        }
    }

    pub fn for_session(
        adapter: TmuxAdapter<R>,
        session_id: impl Into<String>,
    ) -> Result<Self, RealTestError> {
        let session_id = session_id.into();
        validate_real_test_session_id(&session_id)?;
        Ok(Self {
            adapter,
            session_id,
            generation: 0,
            session: None,
            windows: BTreeMap::new(),
            panes: BTreeMap::new(),
            identity_index: BTreeMap::new(),
        })
    }

    pub fn allocate(&mut self, data_point: &DataPoint) -> Result<RealTestLease, RealTestError> {
        let session_id = self.session_id.clone();
        self.allocate_in_session(session_id, data_point)
    }

    pub fn allocate_in_session(
        &mut self,
        session_id: impl Into<String>,
        data_point: &DataPoint,
    ) -> Result<RealTestLease, RealTestError> {
        let topology = RealTestTopology::new(session_id, data_point)?;
        if let Some(lease) = self.lease_for_identity(topology.identity()) {
            return Ok(lease.clone());
        }

        let (window, pane) = match self.session.clone() {
            Some(session) => match self.windows.get(topology.flow_slug()) {
                Some(window) => {
                    let pane = self
                        .adapter
                        .split_pane_for_activation(window, topology.pane_label())?;
                    (window.clone(), pane)
                }
                None => {
                    let (window, pane) = self.adapter.create_window_named_with_pane(
                        &session,
                        topology.flow_slug(),
                        topology.window_name(),
                        topology.pane_label(),
                    )?;
                    self.windows
                        .insert(topology.flow_slug().to_string(), window.clone());
                    (window, pane)
                }
            },
            None => {
                let (session, window, pane) = self.adapter.create_session_with_window_pane(
                    topology.session_id(),
                    topology.flow_slug(),
                    topology.window_name(),
                    topology.pane_label(),
                )?;
                self.advance_generation();
                self.session = Some(session);
                self.windows
                    .insert(topology.flow_slug().to_string(), window.clone());
                (window, pane)
            }
        };
        let lease = topology.lease(window.id(), pane.id(), self.generation);
        self.identity_index
            .insert(topology.identity().to_string(), lease.pane_id().to_string());
        self.panes
            .insert(lease.pane_id().to_string(), lease.clone());

        Ok(lease)
    }

    pub fn release_pane(&mut self, lease: &RealTestLease) -> Result<(), RealTestError> {
        validate_real_test_session_id(lease.session_id())?;
        let owned = self
            .owned_pane(lease)
            .ok_or_else(|| RealTestError::UnownedPane {
                pane_id: lease.pane_id().to_string(),
            })?
            .clone();
        let pane = owned.tmux_pane();
        self.adapter.kill_pane(&pane)?;
        self.panes.remove(owned.pane_id());
        self.identity_index.remove(&owned.identity());
        if !self
            .panes
            .values()
            .any(|remaining| remaining.window_id() == owned.window_id())
        {
            self.windows.remove(owned.flow_slug());
        }
        self.clear_session_if_no_windows();
        Ok(())
    }

    pub fn release_window(&mut self, lease: &RealTestLease) -> Result<(), RealTestError> {
        validate_real_test_session_id(lease.session_id())?;
        let window = self
            .owned_window(lease)
            .ok_or_else(|| RealTestError::UnownedWindow {
                window_id: lease.window_id().to_string(),
            })?
            .clone();
        self.adapter.kill_window(&lease.tmux_window())?;
        let mut removed_identities = Vec::new();
        self.panes.retain(|_, owned| {
            if owned.window_id() == window.id() {
                removed_identities.push(owned.identity());
                false
            } else {
                true
            }
        });
        for identity in removed_identities {
            self.identity_index.remove(&identity);
        }
        self.windows.remove(window.run_id());
        self.clear_session_if_no_windows();
        Ok(())
    }

    pub fn release_session(&mut self) -> Result<(), RealTestError> {
        validate_real_test_session_id(&self.session_id)?;
        let session = self.session.clone().ok_or(RealTestError::UnownedSession)?;
        self.adapter.kill_session(&session)?;
        self.clear_ownership();
        Ok(())
    }

    fn owned_pane(&self, lease: &RealTestLease) -> Option<&RealTestLease> {
        self.session.as_ref()?;
        if lease.generation != self.generation {
            return None;
        }
        let owned = self.panes.get(lease.pane_id())?;
        if owned == lease { Some(owned) } else { None }
    }

    fn owned_window(&self, lease: &RealTestLease) -> Option<&TmuxWindow> {
        let session = self.session.as_ref()?;
        if session.id() != lease.session_id() || lease.generation != self.generation {
            return None;
        }
        let window = self.windows.get(lease.flow_slug())?;
        if window.session_id() == lease.session_id()
            && window.id() == lease.window_id()
            && window.name() == lease.window_name()
            && window.run_id() == lease.flow_slug()
        {
            Some(window)
        } else {
            None
        }
    }

    fn lease_for_identity(&self, identity: &str) -> Option<&RealTestLease> {
        let pane_id = self.identity_index.get(identity)?;
        self.panes.get(pane_id)
    }

    fn advance_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn clear_session_if_no_windows(&mut self) {
        if self.windows.is_empty() {
            self.session = None;
        }
    }

    fn clear_ownership(&mut self) {
        self.windows.clear();
        self.panes.clear();
        self.identity_index.clear();
        self.session = None;
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RealTestError {
    InvalidSession { session_id: String },
    UnownedSession,
    UnownedWindow { window_id: String },
    UnownedPane { pane_id: String },
    Tmux(TmuxError),
}

impl fmt::Display for RealTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSession { session_id } => write!(
                formatter,
                "real-test topology requires session {REAL_TEST_SESSION_ID}, got {session_id}"
            ),
            Self::UnownedSession => write!(
                formatter,
                "real-test session is not owned by this allocator"
            ),
            Self::UnownedWindow { window_id } => write!(
                formatter,
                "real-test window {window_id} is not owned by this allocator"
            ),
            Self::UnownedPane { pane_id } => write!(
                formatter,
                "real-test pane {pane_id} is not owned by this allocator"
            ),
            Self::Tmux(err) => write!(formatter, "{err}"),
        }
    }
}

impl Error for RealTestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidSession { .. } => None,
            Self::UnownedSession => None,
            Self::UnownedWindow { .. } => None,
            Self::UnownedPane { .. } => None,
            Self::Tmux(err) => Some(err),
        }
    }
}

impl From<TmuxError> for RealTestError {
    fn from(err: TmuxError) -> Self {
        Self::Tmux(err)
    }
}

fn validate_real_test_session_id(session_id: &str) -> Result<(), RealTestError> {
    if session_id == REAL_TEST_SESSION_ID {
        Ok(())
    } else {
        Err(RealTestError::InvalidSession {
            session_id: session_id.to_string(),
        })
    }
}
