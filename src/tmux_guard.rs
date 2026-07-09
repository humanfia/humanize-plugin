use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TmuxSendGuardDecision {
    Allowed,
    Blocked(TmuxSendBlock),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxSendBlock {
    pub target: String,
    pub reason: String,
}

pub fn classify_tmux_send(
    argv: &[String],
    owned_panes: &BTreeSet<String>,
) -> TmuxSendGuardDecision {
    classify_tmux_send_with_context(argv, owned_panes, None)
}

pub fn classify_tmux_send_with_context(
    argv: &[String],
    owned_panes: &BTreeSet<String>,
    current_pane: Option<&str>,
) -> TmuxSendGuardDecision {
    let Some(command_index) = tmux_command_index(argv) else {
        return TmuxSendGuardDecision::Allowed;
    };
    let command = argv[command_index].as_str();
    if !is_send_keys_command(command) {
        return TmuxSendGuardDecision::Allowed;
    }

    let target = match send_target(&argv[command_index + 1..]) {
        Some(target) => target,
        None => match current_pane {
            Some(current_pane) => current_pane.to_string(),
            None => return TmuxSendGuardDecision::Allowed,
        },
    };
    if !owned_target_matches(&target, owned_panes) {
        return TmuxSendGuardDecision::Allowed;
    }

    TmuxSendGuardDecision::Blocked(TmuxSendBlock {
        reason: format!(
            "Direct tmux send to Humanize-owned pane {target} is blocked. Use Humanize MCP or the Humanize input tool so machine input is recorded."
        ),
        target,
    })
}

fn owned_target_matches(target: &str, owned_panes: &BTreeSet<String>) -> bool {
    owned_panes.iter().any(|owned| {
        owned == target
            || pane_id_alias(owned).is_some_and(|pane_id| pane_id == target)
            || pane_id_alias(target).is_some_and(|pane_id| pane_id == owned)
            || target_is_parent_of_owned(target, owned)
    })
}

fn target_is_parent_of_owned(target: &str, owned: &str) -> bool {
    if let Some(window) = window_target(owned) {
        if target == window {
            return true;
        }
        if window_id_alias(window).is_some_and(|window_id| target == window_id) {
            return true;
        }
    }

    session_target(owned).is_some_and(|session| target == session)
}

fn pane_id_alias(target: &str) -> Option<&str> {
    if target.starts_with('%') {
        return Some(target);
    }
    target.rsplit_once('.').map(|(_, pane_id)| pane_id)
}

fn window_target(target: &str) -> Option<&str> {
    target
        .rsplit_once('.')
        .map(|(window, _)| window)
        .filter(|window| !window.is_empty())
}

fn window_id_alias(window: &str) -> Option<&str> {
    window
        .rsplit_once(':')
        .map_or(Some(window), |(_, id)| (!id.is_empty()).then_some(id))
}

fn session_target(target: &str) -> Option<&str> {
    target
        .split_once(':')
        .map(|(session, _)| session)
        .filter(|session| !session.is_empty())
}

fn is_send_keys_command(command: &str) -> bool {
    command == "send"
        || command == "send-keys"
        || ("send-keys".starts_with(command) && command.len() >= "send-k".len())
}

fn tmux_command_index(argv: &[String]) -> Option<usize> {
    if !argv.first().is_some_and(|program| is_tmux_program(program)) {
        return None;
    }

    let mut index = 1;
    while index < argv.len() {
        let arg = argv[index].as_str();
        if arg == "--" {
            return (index + 1 < argv.len()).then_some(index + 1);
        }
        if global_flag_takes_value(arg) {
            index += 2;
            continue;
        }
        if global_flag_has_attached_value(arg) || global_flag_without_value(arg) {
            index += 1;
            continue;
        }
        return Some(index);
    }

    None
}

fn is_tmux_program(value: &str) -> bool {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "tmux")
}

fn global_flag_takes_value(arg: &str) -> bool {
    matches!(arg, "-c" | "-f" | "-L" | "-S" | "-T")
}

fn global_flag_has_attached_value(arg: &str) -> bool {
    matches!(
        arg.as_bytes(),
        [b'-', b'c', ..]
            | [b'-', b'f', ..]
            | [b'-', b'L', ..]
            | [b'-', b'S', ..]
            | [b'-', b'T', ..]
    )
}

fn global_flag_without_value(arg: &str) -> bool {
    let Some(flags) = arg.strip_prefix('-') else {
        return false;
    };
    !flags.is_empty()
        && flags.bytes().all(|flag| {
            matches!(
                flag,
                b'2' | b'C' | b'D' | b'h' | b'l' | b'N' | b'u' | b'v' | b'V'
            )
        })
}

fn send_target(args: &[String]) -> Option<String> {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--" {
            return None;
        }
        if arg == "-t" {
            return args.get(index + 1).cloned();
        }
        if let Some(target) = arg.strip_prefix("-t=") {
            return (!target.is_empty()).then(|| target.to_string());
        }
        if let Some(target) = arg.strip_prefix("-t") {
            return (!target.is_empty()).then(|| target.to_string());
        }
        index += 1;
    }

    None
}
