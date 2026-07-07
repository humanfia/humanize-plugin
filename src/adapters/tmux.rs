use std::error::Error;
use std::fmt;
use std::process::Command;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxAdapter<R: CommandRunner = SystemCommandRunner> {
    runner: R,
}

impl TmuxAdapter<SystemCommandRunner> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for TmuxAdapter<SystemCommandRunner> {
    fn default() -> Self {
        Self {
            runner: SystemCommandRunner,
        }
    }
}

impl<R: CommandRunner> TmuxAdapter<R> {
    pub fn with_runner(runner: R) -> Self {
        Self { runner }
    }

    pub fn create_session_with_window_pane(
        &self,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
        activation_id: impl Into<String>,
    ) -> Result<(TmuxSession, TmuxWindow, TmuxPane), TmuxError> {
        let session = TmuxSession::new(session_id);
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let activation_id = activation_id.into();
        let output = self.run_checked(argv(
            [
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}\t#{pane_id}",
                "-s",
                session.id(),
                "-n",
            ],
            [window_name.as_str()],
        ))?;
        let (window_id, pane_id) = window_pane_stdout(&output)?;
        let window = TmuxWindow::new_named(session.id(), run_id, window_name, window_id);
        let pane = TmuxPane::new_in_session(
            session.id(),
            window.id(),
            activation_id.as_str(),
            pane_id.as_str(),
        );

        Ok((session, window, pane))
    }

    pub fn ensure_session(&self, session_id: impl Into<String>) -> Result<TmuxSession, TmuxError> {
        let session = TmuxSession::new(session_id);
        validate_session_id(session.id())?;
        let check = self
            .runner
            .run(argv(["tmux", "has-session", "-t"], [session.id()]))?;

        if check.is_success() {
            return Ok(session);
        }

        self.run_checked(argv(["tmux", "new-session", "-d", "-s"], [session.id()]))?;
        Ok(session)
    }

    pub fn create_window(
        &self,
        session: &TmuxSession,
        run_id: impl Into<String>,
    ) -> Result<TmuxWindow, TmuxError> {
        let run_id = run_id.into();
        self.create_window_named(session, run_id.clone(), run_id)
    }

    pub fn create_window_named(
        &self,
        session: &TmuxSession,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
    ) -> Result<TmuxWindow, TmuxError> {
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let output = self.run_checked(argv(
            [
                "tmux",
                "new-window",
                "-P",
                "-F",
                "#{window_id}",
                "-t",
                session.id(),
                "-n",
            ],
            [window_name.as_str()],
        ))?;
        let window_id = trimmed_stdout(&output, "window id")?;

        Ok(TmuxWindow::new_named(
            session.id(),
            run_id,
            window_name,
            window_id,
        ))
    }

    pub fn create_window_named_with_pane(
        &self,
        session: &TmuxSession,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
        activation_id: impl Into<String>,
    ) -> Result<(TmuxWindow, TmuxPane), TmuxError> {
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let activation_id = activation_id.into();
        let output = self.run_checked(argv(
            [
                "tmux",
                "new-window",
                "-P",
                "-F",
                "#{window_id}\t#{pane_id}",
                "-t",
                session.id(),
                "-n",
            ],
            [window_name.as_str()],
        ))?;
        let (window_id, pane_id) = window_pane_stdout(&output)?;
        let window = TmuxWindow::new_named(session.id(), run_id, window_name, window_id);
        let pane = TmuxPane::new_in_session(
            session.id(),
            window.id(),
            activation_id.as_str(),
            pane_id.as_str(),
        );

        Ok((window, pane))
    }

    pub fn split_pane_for_activation(
        &self,
        window: &TmuxWindow,
        activation_id: impl Into<String>,
    ) -> Result<TmuxPane, TmuxError> {
        validate_owned_session_id("window", window.session_id())?;
        let activation_id = activation_id.into();
        let target = window_target(window);
        let output = self.run_checked(argv(
            [
                "tmux",
                "split-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                target.as_str(),
            ],
            ["-v"],
        ))?;
        let pane_id = trimmed_stdout(&output, "pane id")?;

        Ok(TmuxPane::new_in_session(
            window.session_id(),
            window.id(),
            activation_id,
            pane_id,
        ))
    }

    pub fn kill_pane(&self, pane: &TmuxPane) -> Result<(), TmuxError> {
        validate_owned_session_id("pane", pane.session_id())?;
        let target = pane_target(pane);
        self.run_checked(argv(["tmux", "kill-pane", "-t"], [target.as_str()]))?;
        Ok(())
    }

    pub fn kill_window(&self, window: &TmuxWindow) -> Result<(), TmuxError> {
        validate_owned_session_id("window", window.session_id())?;
        let target = window_target(window);
        self.run_checked(argv(["tmux", "kill-window", "-t"], [target.as_str()]))?;
        Ok(())
    }

    pub fn kill_session(&self, session: &TmuxSession) -> Result<(), TmuxError> {
        validate_owned_session_id("session", session.id())?;
        self.run_checked(argv(["tmux", "kill-session", "-t"], [session.id()]))?;
        Ok(())
    }

    pub fn send_keys_literal(&self, pane: &TmuxPane, text: &str) -> Result<(), TmuxError> {
        validate_owned_session_id("pane", pane.session_id())?;
        let target = pane_target(pane);
        self.run_checked(argv(
            ["tmux", "send-keys", "-t", target.as_str(), "-l"],
            [text],
        ))?;
        Ok(())
    }

    pub fn capture_pane(&self, pane: &TmuxPane) -> Result<String, TmuxError> {
        validate_owned_session_id("pane", pane.session_id())?;
        let target = pane_target(pane);
        let output = self.run_checked(argv(
            ["tmux", "capture-pane", "-p", "-t"],
            [target.as_str()],
        ))?;
        Ok(output.stdout)
    }

    fn run_checked(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        let output = self.runner.run(argv.clone())?;
        if output.is_success() {
            Ok(output)
        } else {
            Err(TmuxError::CommandFailed {
                argv,
                status: output.status,
                stderr: output.stderr,
            })
        }
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct TmuxSession {
    id: String,
}

impl TmuxSession {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct TmuxWindow {
    session_id: String,
    run_id: String,
    name: String,
    id: String,
}

impl TmuxWindow {
    pub fn new(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        let run_id = run_id.into();
        Self {
            session_id: session_id.into(),
            name: run_id.clone(),
            run_id,
            id: id.into(),
        }
    }

    pub fn new_named(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        name: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            run_id: run_id.into(),
            name: name.into(),
            id: id.into(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct TmuxPane {
    session_id: String,
    window_id: String,
    activation_id: String,
    id: String,
}

impl TmuxPane {
    pub fn new(
        window_id: impl Into<String>,
        activation_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: String::new(),
            window_id: window_id.into(),
            activation_id: activation_id.into(),
            id: id.into(),
        }
    }

    pub fn new_in_session(
        session_id: impl Into<String>,
        window_id: impl Into<String>,
        activation_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            window_id: window_id.into(),
            activation_id: activation_id.into(),
            id: id.into(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn window_id(&self) -> &str {
        &self.window_id
    }

    pub fn activation_id(&self) -> &str {
        &self.activation_id
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct PaneActivation {
    activation_id: String,
    pane_id: String,
}

impl PaneActivation {
    pub fn new(activation_id: impl Into<String>, pane_id: impl Into<String>) -> Self {
        Self {
            activation_id: activation_id.into(),
            pane_id: pane_id.into(),
        }
    }

    pub fn activation_id(&self) -> &str {
        &self.activation_id
    }

    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }
}

pub trait CommandRunner: Clone {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError>;
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            status: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    pub fn failure(stderr: impl Into<String>) -> Self {
        Self {
            status: 1,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    pub fn success_status(
        status: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            status,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    pub fn is_success(&self) -> bool {
        self.status == 0
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        let Some((program, args)) = argv.split_first() else {
            return Err(TmuxError::EmptyArgv);
        };

        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|err| TmuxError::Io {
                argv,
                message: err.to_string(),
            })?;
        let status = output.status.code().unwrap_or(-1);

        Ok(CommandOutput {
            status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TmuxError {
    EmptyArgv,
    MissingSession {
        target: &'static str,
    },
    ReservedSession {
        session_id: String,
    },
    EmptyOutput {
        field: &'static str,
    },
    Io {
        argv: Vec<String>,
        message: String,
    },
    CommandFailed {
        argv: Vec<String>,
        status: i32,
        stderr: String,
    },
}

impl fmt::Display for TmuxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyArgv => write!(formatter, "empty command argv"),
            Self::MissingSession { target } => {
                write!(formatter, "tmux {target} requires session ownership")
            }
            Self::ReservedSession { session_id } => {
                write!(formatter, "tmux session named {session_id} is reserved")
            }
            Self::EmptyOutput { field } => write!(formatter, "tmux did not return {field}"),
            Self::Io { argv, message } => write!(formatter, "{}: {message}", argv.join(" ")),
            Self::CommandFailed {
                argv,
                status,
                stderr,
            } => write!(
                formatter,
                "{} failed with status {status}: {stderr}",
                argv.join(" ")
            ),
        }
    }
}

impl Error for TmuxError {}

fn argv<const N: usize, const M: usize>(head: [&str; N], tail: [&str; M]) -> Vec<String> {
    head.into_iter().chain(tail).map(String::from).collect()
}

fn validate_session_id(session_id: &str) -> Result<(), TmuxError> {
    if session_id == "dev" {
        Err(TmuxError::ReservedSession {
            session_id: session_id.to_string(),
        })
    } else {
        Ok(())
    }
}

fn validate_owned_session_id(target: &'static str, session_id: &str) -> Result<(), TmuxError> {
    if session_id.is_empty() {
        Err(TmuxError::MissingSession { target })
    } else {
        validate_session_id(session_id)
    }
}

fn window_target(window: &TmuxWindow) -> String {
    format!("{}:{}", window.session_id(), window.id())
}

fn pane_target(pane: &TmuxPane) -> String {
    format!("{}:{}.{}", pane.session_id(), pane.window_id(), pane.id())
}

fn trimmed_stdout(output: &CommandOutput, field: &'static str) -> Result<String, TmuxError> {
    let value = output.stdout.trim();
    if value.is_empty() {
        Err(TmuxError::EmptyOutput { field })
    } else {
        Ok(value.to_string())
    }
}

fn window_pane_stdout(output: &CommandOutput) -> Result<(String, String), TmuxError> {
    let value = output.stdout.trim();
    if value.is_empty() {
        return Err(TmuxError::EmptyOutput {
            field: "window and pane ids",
        });
    }

    let mut fields = value.split_whitespace();
    let Some(window_id) = fields.next() else {
        return Err(TmuxError::EmptyOutput { field: "window id" });
    };
    let Some(pane_id) = fields.next() else {
        return Err(TmuxError::EmptyOutput { field: "pane id" });
    };

    Ok((window_id.to_string(), pane_id.to_string()))
}
