use std::str::FromStr;

use humanize_plugin::client_config::{ClientConfigTarget, render_client_config};
use serde_json::{Value, json};

const COMMAND: &str = "/opt/humanize-plugin-mcp";

#[test]
fn renders_exact_snippets_for_supported_targets() {
    let cases = [
        (
            ClientConfigTarget::CodexSession,
            concat!(
                "codex -C \"$PWD\" \\\n",
                "  -c 'mcp_servers.humanize_plugin.command=\"/opt/humanize-plugin-mcp\"' \\\n",
                "  -c 'mcp_servers.humanize_plugin.args=[]'"
            ),
        ),
        (
            ClientConfigTarget::CodexPersistent,
            "codex mcp add humanize_plugin -- /opt/humanize-plugin-mcp",
        ),
        (
            ClientConfigTarget::ClaudeProject,
            "claude mcp add --scope project humanize_plugin -- /opt/humanize-plugin-mcp",
        ),
        (
            ClientConfigTarget::ClaudeSessionJson,
            concat!(
                "{\n",
                "  \"mcpServers\": {\n",
                "    \"humanize_plugin\": {\n",
                "      \"command\": \"/opt/humanize-plugin-mcp\",\n",
                "      \"args\": []\n",
                "    }\n",
                "  }\n",
                "}"
            ),
        ),
    ];

    for (target, expected) in cases {
        assert_eq!(render_client_config(target, COMMAND).unwrap(), expected);
    }
}

#[test]
fn session_json_parses_with_expected_server_shape() {
    let rendered = render_client_config(ClientConfigTarget::ClaudeSessionJson, COMMAND).unwrap();
    let parsed: Value = serde_json::from_str(&rendered).unwrap();

    assert_eq!(
        parsed["mcpServers"]["humanize_plugin"]["command"],
        json!(COMMAND)
    );
    assert_eq!(parsed["mcpServers"]["humanize_plugin"]["args"], json!([]));
}

#[test]
fn shell_snippets_quote_paths_with_spaces_and_single_quotes() {
    let command = "/opt/Humanize Plugin/o'hare/bin/humanize-plugin-mcp";

    assert_eq!(
        render_client_config(ClientConfigTarget::CodexPersistent, command).unwrap(),
        "codex mcp add humanize_plugin -- '/opt/Humanize Plugin/o'\\''hare/bin/humanize-plugin-mcp'"
    );
    assert_eq!(
        render_client_config(ClientConfigTarget::ClaudeProject, command).unwrap(),
        "claude mcp add --scope project humanize_plugin -- '/opt/Humanize Plugin/o'\\''hare/bin/humanize-plugin-mcp'"
    );
    assert_eq!(
        render_client_config(ClientConfigTarget::CodexSession, command).unwrap(),
        concat!(
            "codex -C \"$PWD\" \\\n",
            "  -c 'mcp_servers.humanize_plugin.command=\"/opt/Humanize Plugin/o'\\''hare/bin/humanize-plugin-mcp\"' \\\n",
            "  -c 'mcp_servers.humanize_plugin.args=[]'"
        )
    );
}

#[test]
fn target_names_parse_to_expected_variants() {
    let cases = [
        ("codex-session", ClientConfigTarget::CodexSession),
        ("codex-persistent", ClientConfigTarget::CodexPersistent),
        ("claude-project", ClientConfigTarget::ClaudeProject),
        ("claude-session-json", ClientConfigTarget::ClaudeSessionJson),
    ];

    for (name, target) in cases {
        assert_eq!(ClientConfigTarget::from_str(name).unwrap(), target);
    }
}

#[test]
fn empty_command_and_unknown_target_return_errors() {
    assert!(render_client_config(ClientConfigTarget::CodexSession, "").is_err());
    assert!(render_client_config(ClientConfigTarget::CodexSession, "   ").is_err());
    assert!(ClientConfigTarget::from_str("unknown-target").is_err());
}
