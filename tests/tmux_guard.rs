use std::collections::BTreeSet;

use humanize_plugin::tmux_guard::{
    TmuxSendBlock, TmuxSendGuardDecision, classify_tmux_send, classify_tmux_send_with_context,
};

fn argv(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn owned_panes(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn blocks_tmux_send_keys_to_owned_target() {
    let decision = classify_tmux_send(
        &argv(&[
            "tmux",
            "send-keys",
            "-t",
            "host-a:%7.%8",
            "-l",
            "inspect the repo",
        ]),
        &owned_panes(&["host-a:%7.%8"]),
    );

    assert_eq!(
        decision,
        TmuxSendGuardDecision::Blocked(TmuxSendBlock {
            target: "host-a:%7.%8".to_string(),
            reason: "Direct tmux send to Humanize-owned pane host-a:%7.%8 is blocked. Use Humanize MCP or the Humanize input tool so machine input is recorded."
                .to_string(),
        })
    );
}

#[test]
fn blocks_tmux_send_alias_after_global_flags() {
    let decision = classify_tmux_send(
        &argv(&[
            "tmux",
            "-L",
            "humanize-test",
            "-S",
            "/tmp/humanize-tmux.sock",
            "send",
            "-t",
            "%12",
            "Enter",
        ]),
        &owned_panes(&["%12"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%12"));
}

#[test]
fn blocks_tmux_send_with_attached_global_and_target_options() {
    let decision = classify_tmux_send(
        &argv(&[
            "/usr/bin/tmux",
            "-Lhumanize-test",
            "send-keys",
            "-t=%12",
            "-l",
            "inspect the repo",
        ]),
        &owned_panes(&["%12"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%12"));

    let decision = classify_tmux_send(
        &argv(&["tmux", "send", "-t%13", "Enter"]),
        &owned_panes(&["%13"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%13"));
}

#[test]
fn blocks_tmux_send_after_value_taking_global_flags() {
    let decision = classify_tmux_send(
        &argv(&[
            "tmux",
            "-c",
            "/tmp",
            "-Tfeatures",
            "send-key",
            "-t",
            "%12",
            "Enter",
        ]),
        &owned_panes(&["%12"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%12"));
}

#[test]
fn blocks_tmux_send_after_combined_no_value_global_flags() {
    let decision = classify_tmux_send(
        &argv(&[
            "tmux",
            "-vv",
            "-CC",
            "send-key",
            "-t",
            "host-a:%7.%8",
            "Enter",
        ]),
        &owned_panes(&["host-a:%7.%8"]),
    );

    assert!(
        matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "host-a:%7.%8")
    );
}

#[test]
fn blocks_tmux_send_to_owned_pane_id_alias() {
    let decision = classify_tmux_send(
        &argv(&["tmux", "send-keys", "-t", "%8", "-l", "inspect the repo"]),
        &owned_panes(&["host-a:%7.%8"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%8"));

    let decision = classify_tmux_send(
        &argv(&["tmux", "send-keys", "-t", "host-a:%7.%9", "Enter"]),
        &owned_panes(&["%9"]),
    );

    assert!(
        matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "host-a:%7.%9")
    );
}

#[test]
fn blocks_tmux_send_to_owned_window_or_session_target() {
    let decision = classify_tmux_send(
        &argv(&["tmux", "send-keys", "-t", "host-a:%7", "Enter"]),
        &owned_panes(&["host-a:%7.%8"]),
    );

    assert!(
        matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "host-a:%7")
    );

    let decision = classify_tmux_send(
        &argv(&["tmux", "send-keys", "-t", "host-a", "Enter"]),
        &owned_panes(&["host-a:%7.%8"]),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "host-a"));
}

#[test]
fn blocks_tmux_send_without_target_when_current_pane_is_owned() {
    let decision = classify_tmux_send_with_context(
        &argv(&["tmux", "send-keys", "-l", "inspect the repo"]),
        &owned_panes(&["host-a:%7.%8"]),
        Some("%8"),
    );

    assert!(matches!(decision, TmuxSendGuardDecision::Blocked(block) if block.target == "%8"));
}

#[test]
fn allows_tmux_send_without_owned_explicit_target() {
    assert_eq!(
        classify_tmux_send(
            &argv(&["tmux", "send-keys", "-l", "inspect the repo"]),
            &owned_panes(&["host-a:%7.%8"]),
        ),
        TmuxSendGuardDecision::Allowed
    );
    assert_eq!(
        classify_tmux_send(
            &argv(&["tmux", "send-keys", "-t", "other:%1.%2", "Enter"]),
            &owned_panes(&["host-a:%7.%8"]),
        ),
        TmuxSendGuardDecision::Allowed
    );
}

#[test]
fn allows_unknown_or_non_send_commands() {
    let owned = owned_panes(&["host-a:%7.%8"]);

    assert_eq!(
        classify_tmux_send(
            &argv(&["tmux", "capture-pane", "-t", "host-a:%7.%8"]),
            &owned,
        ),
        TmuxSendGuardDecision::Allowed
    );
    assert_eq!(
        classify_tmux_send(
            &argv(&["ssh", "host", "tmux", "send-keys", "-t", "host-a:%7.%8"]),
            &owned,
        ),
        TmuxSendGuardDecision::Allowed
    );
}
