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
