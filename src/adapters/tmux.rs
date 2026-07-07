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

    pub fn ensure_session(&self, session_id: impl Into<String>) -> Result<TmuxSession, TmuxError> {
        let session = TmuxSession::new(session_id);
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
            [run_id.as_str()],
        ))?;
        let window_id = trimmed_stdout(&output, "window id")?;

        Ok(TmuxWindow::new(session.id(), run_id, window_id))
    }

    pub fn split_pane_for_activation(
        &self,
        window: &TmuxWindow,
        activation_id: impl Into<String>,
    ) -> Result<TmuxPane, TmuxError> {
        let activation_id = activation_id.into();
        let output = self.run_checked(argv(
            [
                "tmux",
                "split-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                window.id(),
            ],
            ["-v"],
        ))?;
        let pane_id = trimmed_stdout(&output, "pane id")?;

        Ok(TmuxPane::new(window.id(), activation_id, pane_id))
    }

    pub fn send_keys_literal(&self, pane: &TmuxPane, text: &str) -> Result<(), TmuxError> {
        self.run_checked(argv(["tmux", "send-keys", "-t", pane.id(), "-l"], [text]))?;
        Ok(())
    }

    pub fn capture_pane(&self, pane: &TmuxPane) -> Result<String, TmuxError> {
        let output = self.run_checked(argv(["tmux", "capture-pane", "-p", "-t"], [pane.id()]))?;
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
    id: String,
}

impl TmuxWindow {
    pub fn new(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            run_id: run_id.into(),
            id: id.into(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct TmuxPane {
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
            window_id: window_id.into(),
            activation_id: activation_id.into(),
            id: id.into(),
        }
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

fn trimmed_stdout(output: &CommandOutput, field: &'static str) -> Result<String, TmuxError> {
    let value = output.stdout.trim();
    if value.is_empty() {
        Err(TmuxError::EmptyOutput { field })
    } else {
        Ok(value.to_string())
    }
}
