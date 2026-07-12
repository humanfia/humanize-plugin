mod support;

use std::fs;
use std::path::PathBuf;

use humanize_plugin::mcp::McpServer;
use serde_json::Value;
use serde_json::json;

use support::mcp::{RecordingRunner, call_tool, structured};

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_text(relative: &str) -> String {
    fs::read_to_string(repo_path(relative)).unwrap_or_else(|error| {
        panic!("failed to read {relative}: {error}");
    })
}

fn read_json(relative: &str) -> Value {
    serde_json::from_str(&read_text(relative)).unwrap_or_else(|error| {
        panic!("failed to parse {relative} as JSON: {error}");
    })
}

#[test]
fn codex_distribution_metadata_points_at_repo_root_package() {
    let marketplace = read_json(".agents/plugins/marketplace.json");
    let plugin = read_json(".codex-plugin/plugin.json");
    let package = marketplace["plugins"]
        .as_array()
        .expect("Codex marketplace plugins should be an array")
        .iter()
        .find(|candidate| candidate["name"] == "humanize-plugin")
        .expect("Codex marketplace should list humanize-plugin");

    assert_eq!(marketplace["name"], "humanfia");
    assert_eq!(package["name"], "humanize-plugin");
    assert_eq!(package["source"]["source"], "local");
    assert_eq!(package["source"]["path"], "./");
    assert_eq!(package["policy"]["installation"], "AVAILABLE");
    assert_eq!(package["policy"]["authentication"], "ON_INSTALL");

    assert_eq!(plugin["name"], "humanize-plugin");
    assert_eq!(plugin["mcpServers"], "./.mcp.json");
    assert_eq!(plugin["skills"], "./skills/");
    assert_eq!(
        plugin["repository"],
        "https://github.com/humanfia/humanize-plugin"
    );
}

#[test]
fn mcp_server_metadata_uses_published_binary_name() {
    let mcp = read_json(".mcp.json");
    let server = &mcp["mcpServers"]["humanize_plugin"];

    assert_eq!(server["command"], "humanize-plugin-mcp");
    assert_eq!(
        server["args"]
            .as_array()
            .expect("humanize_plugin args should be an array")
            .len(),
        0
    );
}

#[test]
fn claude_distribution_metadata_reuses_shared_mcp_and_skills() {
    let plugin = read_json(".claude-plugin/plugin.json");
    let marketplace = read_json(".claude-plugin/marketplace.json");
    let package = marketplace["plugins"]
        .as_array()
        .expect("Claude marketplace plugins should be an array")
        .iter()
        .find(|candidate| candidate["name"] == "humanize-plugin")
        .expect("Claude marketplace should list humanize-plugin");

    assert_eq!(plugin["name"], "humanize-plugin");
    assert_eq!(plugin["mcpServers"], "./.mcp.json");
    assert_eq!(plugin["skills"], "./skills/");
    assert_eq!(marketplace["name"], "humanfia");
    assert_eq!(package["name"], "humanize-plugin");
    assert_eq!(package["source"], "./");
}

#[test]
fn readme_starts_with_production_install_flow_and_terse_prompt() {
    let readme = read_text("README.md");
    let beginning: String = readme.chars().take(1_800).collect();
    let install_heading = beginning
        .find("## Install\n")
        .expect("README beginning should include the production Install heading");
    let runtime_install = beginning
        .find("cargo install --git https://github.com/humanfia/humanize-plugin --locked --bin humanize-plugin-mcp")
        .expect("README beginning should include the production runtime install command");
    let codex_marketplace = beginning
        .find("codex plugin marketplace add humanfia/humanize-plugin")
        .expect("README beginning should include the Codex marketplace command");
    let codex_install = beginning
        .find("codex plugin add humanize-plugin@humanfia")
        .expect("README beginning should include the Codex install command");
    let claude_marketplace = beginning
        .find("claude plugin marketplace add humanfia/humanize-plugin")
        .expect("README beginning should include the Claude Code marketplace command");
    let claude_install = beginning
        .find("claude plugin install humanize-plugin@humanfia")
        .expect("README beginning should include the Claude Code install command");
    let prompt_heading = beginning
        .find("## Start with natural language")
        .expect("README beginning should include terse prompt examples before details");
    let prompt = beginning
        .find("Use Humanize")
        .expect("README beginning should include a terse Use Humanize prompt");

    assert!(install_heading < runtime_install);
    assert!(runtime_install < codex_marketplace);
    assert!(codex_marketplace < codex_install);
    assert!(codex_install < claude_marketplace);
    assert!(claude_marketplace < claude_install);
    assert!(claude_install < prompt_heading);
    assert!(prompt_heading < prompt);
    assert!(
        !beginning.contains("Install Prerequisites"),
        "README beginning should not push client install behind prerequisites"
    );

    assert!(readme.contains(
        "cargo install --git https://github.com/humanfia/humanize-plugin --locked --bin humanize-plugin-mcp --force"
    ));
    assert!(readme.contains("cargo uninstall humanize-plugin"));
}

#[test]
fn workflow_skill_explains_executable_tmux_agent_runs() {
    let skill = read_text("skills/humanize-workflows/SKILL.md");

    assert!(skill.contains("```sh\nexport HUMANIZE_TMUX_SESSION="));
    assert!(skill.contains("HUMANIZE_TMUX_SESSION"));
    assert!(skill.contains("HUMANIZE_AGENT_COMMAND"));
    assert!(!skill.contains("\"HUMANIZE_TMUX_SESSION\":"));
    assert!(skill.contains("Review nodes are agent-backed"));
    assert!(skill.contains("Script action drivers are rejected before lock"));
    assert!(skill.contains("When calling `deliver_artifact`, use the bare artifact id,"));
    assert!(skill.contains("such as `baseline`, not the fact path `artifact.baseline`."));
    assert!(!skill.contains("script and review nodes require explicit orchestration"));
}

#[test]
fn workflow_skill_minimal_example_keeps_valid_adaptive_review_loop() {
    let skill = read_text("skills/humanize-workflows/SKILL.md");
    let example = fenced_block_after(&skill, "## Minimal Draft Example", "json");
    let flow =
        serde_json::from_str::<Value>(example).expect("minimal draft example should be valid JSON");

    assert!(
        flow["routes"]
            .as_array()
            .expect("minimal draft example should include routes")
            .iter()
            .any(
                |route| route["predicate"] == "exists(artifact.review_continue)"
                    && route["activate"] == "try_candidates"
            )
    );
    assert!(!example.contains("artifact.review_verdict =="));
    let resource_sources = flow["resources"]
        .as_array()
        .expect("minimal draft example should include resources")
        .iter()
        .filter_map(|resource| resource["source"].as_str())
        .collect::<Vec<_>>();
    assert!(
        resource_sources
            .iter()
            .any(|source| source.contains("artifact_key \"baseline\""))
    );
    assert!(
        resource_sources
            .iter()
            .any(|source| source.contains("artifact_key \"candidates\""))
    );
    assert!(
        resource_sources
            .iter()
            .any(|source| source.contains("artifact_key \"review_verdict\""))
    );
    assert!(
        resource_sources
            .iter()
            .any(|source| source.contains("artifact_key \"review_continue\""))
    );
    assert!(!example.contains("Write artifact.review_continue"));

    let mut server = McpServer::with_tmux_runner(RecordingRunner::default());
    let checked = call_tool(
        &mut server,
        1,
        "flow_check",
        json!({
            "flow": flow
        }),
    );

    assert_eq!(structured(&checked)["ok"], true);
    assert_eq!(structured(&checked)["diagnostics"], json!([]));
}

fn fenced_block_after<'a>(text: &'a str, anchor: &str, language: &str) -> &'a str {
    let section = text
        .split_once(anchor)
        .unwrap_or_else(|| panic!("missing section {anchor}"))
        .1;
    let fence = format!("```{language}");
    let after_fence = section
        .split_once(&fence)
        .unwrap_or_else(|| panic!("missing {language} fence after {anchor}"))
        .1;
    after_fence
        .split_once("```")
        .unwrap_or_else(|| panic!("missing closing fence after {anchor}"))
        .0
        .trim()
}
