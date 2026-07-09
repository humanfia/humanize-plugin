use std::process::Command;

use serde_json::{Value, json};

const COMMAND: &str = "/opt/humanize-plugin-mcp";

fn run_plugin(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn cli_prints_codex_session_snippet() {
    let output = run_plugin(&[
        "--print-client-config",
        "codex-session",
        "--command",
        COMMAND,
    ]);

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!(
            "codex -C \"$PWD\" \\\n",
            "  -c 'mcp_servers.humanize_plugin.command=\"/opt/humanize-plugin-mcp\"' \\\n",
            "  -c 'mcp_servers.humanize_plugin.args=[]'\n"
        )
    );
}

#[test]
fn cli_prints_parseable_claude_session_json() {
    let output = run_plugin(&[
        "--print-client-config",
        "claude-session-json",
        "--command",
        COMMAND,
    ]);

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        parsed["mcpServers"]["humanize_plugin"]["command"],
        json!(COMMAND)
    );
    assert_eq!(parsed["mcpServers"]["humanize_plugin"]["args"], json!([]));
}

#[test]
fn cli_rejects_unknown_target() {
    let output = run_plugin(&[
        "--print-client-config",
        "unknown-target",
        "--command",
        COMMAND,
    ]);

    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown client config target"));
    assert!(stderr.contains("usage: humanize-plugin-mcp"));
}

#[test]
fn cli_guard_blocks_owned_tmux_send() {
    let output = run_plugin(&[
        "--guard-tmux-send",
        "--owned-pane",
        "host-a:%7.%8",
        "--",
        "send-keys",
        "-t",
        "host-a:%7.%8",
        "-l",
        "inspect the repo",
    ]);

    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Use Humanize MCP"));
    assert!(stderr.contains("Humanize input tool"));
}

#[test]
fn cli_guard_allows_unowned_tmux_send() {
    let output = run_plugin(&[
        "--guard-tmux-send",
        "--owned-pane",
        "host-a:%7.%8",
        "--",
        "send-keys",
        "-t",
        "other:%1.%2",
        "Enter",
    ]);

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

#[test]
fn cli_guard_blocks_current_owned_pane_when_tmux_send_has_no_target() {
    let output = run_plugin(&[
        "--guard-tmux-send",
        "--owned-pane",
        "host-a:%7.%8",
        "--current-pane",
        "%8",
        "--",
        "send-keys",
        "-l",
        "inspect the repo",
    ]);

    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Direct tmux send to Humanize-owned pane %8 is blocked")
    );
}
