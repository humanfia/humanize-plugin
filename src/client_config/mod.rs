use std::error::Error;
use std::fmt;
use std::str::FromStr;

use serde::Serialize;
use serde_json::json;

mod hooks;

pub use hooks::{run_participant_exited_hook, run_session_start_hook, run_stop_hook};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ClientConfigTarget {
    CodexSession,
    CodexPersistent,
    CodexHooksJson,
    ClaudeProject,
    ClaudeSessionJson,
    ClaudeHooksJson,
}

impl FromStr for ClientConfigTarget {
    type Err = ClientConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "codex-session" => Ok(Self::CodexSession),
            "codex-persistent" => Ok(Self::CodexPersistent),
            "codex-hooks-json" => Ok(Self::CodexHooksJson),
            "claude-project" => Ok(Self::ClaudeProject),
            "claude-session-json" => Ok(Self::ClaudeSessionJson),
            "claude-hooks-json" => Ok(Self::ClaudeHooksJson),
            _ => Err(ClientConfigError::UnknownTarget(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ClientConfigError {
    EmptyCommand,
    UnknownTarget(String),
    JsonRender(String),
}

impl fmt::Display for ClientConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCommand => write!(formatter, "client config command must not be empty"),
            Self::UnknownTarget(target) => {
                write!(formatter, "unknown client config target: {target}")
            }
            Self::JsonRender(err) => {
                write!(formatter, "failed to render client config JSON: {err}")
            }
        }
    }
}

impl Error for ClientConfigError {}

pub fn render_client_config(
    target: ClientConfigTarget,
    command: &str,
) -> Result<String, ClientConfigError> {
    if command.trim().is_empty() {
        return Err(ClientConfigError::EmptyCommand);
    }

    match target {
        ClientConfigTarget::CodexSession => render_codex_session(command),
        ClientConfigTarget::CodexPersistent => Ok(format!(
            "codex mcp add humanize_plugin -- {}",
            shell_word(command)
        )),
        ClientConfigTarget::CodexHooksJson => render_hooks_json(
            command,
            "codex_session_start",
            "--codex-pre-tool-use-hook",
            "--codex-stop-hook",
        ),
        ClientConfigTarget::ClaudeProject => Ok(format!(
            "claude mcp add --scope project humanize_plugin -- {}",
            shell_word(command)
        )),
        ClientConfigTarget::ClaudeSessionJson => render_claude_session_json(command),
        ClientConfigTarget::ClaudeHooksJson => render_hooks_json(
            command,
            "claude_session_start",
            "--claude-pre-tool-use-hook",
            "--claude-stop-hook",
        ),
    }
}

fn render_codex_session(command: &str) -> Result<String, ClientConfigError> {
    let command_value = serde_json::to_string(command)
        .map_err(|err| ClientConfigError::JsonRender(err.to_string()))?;
    let command_arg = shell_single_quoted(&format!(
        "mcp_servers.humanize_plugin.command={command_value}"
    ));
    let args_arg = shell_single_quoted("mcp_servers.humanize_plugin.args=[]");

    Ok(format!(
        "codex -C \"$PWD\" \\\n  -c {command_arg} \\\n  -c {args_arg}"
    ))
}

fn render_claude_session_json(command: &str) -> Result<String, ClientConfigError> {
    let config = ClaudeSessionConfig {
        mcp_servers: McpServers {
            humanize_plugin: ServerConfig {
                command,
                args: Vec::<String>::new(),
            },
        },
    };

    serde_json::to_string_pretty(&config)
        .map_err(|err| ClientConfigError::JsonRender(err.to_string()))
}

fn render_hooks_json(
    command: &str,
    ready_source: &str,
    pre_tool_flag: &str,
    stop_flag: &str,
) -> Result<String, ClientConfigError> {
    let command_word = shell_word(command);
    let ready_command = format!("{command_word} --agent-ready-hook --source {ready_source}");
    let guard_command = format!("{command_word} {pre_tool_flag}");
    let stop_command = format!("{command_word} {stop_flag}");
    serde_json::to_string_pretty(&json!({
        "hooks": {
            "SessionStart": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            "command": ready_command,
                            "statusMessage": "Recording Humanize agent readiness"
                        }
                    ]
                }
            ],
            "PreToolUse": [
                {
                    "matcher": "Bash",
                    "hooks": [
                        {
                            "type": "command",
                            "command": guard_command,
                            "statusMessage": "Checking Humanize tmux ownership"
                        }
                    ]
                }
            ],
            "Stop": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            "command": stop_command,
                            "statusMessage": "Validating Humanize participant completion"
                        }
                    ]
                }
            ]
        }
    }))
    .map_err(|err| ClientConfigError::JsonRender(err.to_string()))
}

fn shell_word(value: &str) -> String {
    if value.chars().all(is_safe_shell_word) {
        value.to_owned()
    } else {
        shell_single_quoted(value)
    }
}

fn is_safe_shell_word(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '/' | '.' | '_' | '-' | '+' | '=' | ':')
}

fn shell_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Serialize)]
struct ClaudeSessionConfig<'a> {
    #[serde(rename = "mcpServers")]
    mcp_servers: McpServers<'a>,
}

#[derive(Serialize)]
struct McpServers<'a> {
    humanize_plugin: ServerConfig<'a>,
}

#[derive(Serialize)]
struct ServerConfig<'a> {
    command: &'a str,
    args: Vec<String>,
}
