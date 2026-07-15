use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::run_assets::{
    OwnedTmuxPane, RunAssetError, TmuxGuardBlockedEvidence, discover_live_owned_tmux_panes_in_dir,
};

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

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ShellTmuxGuardDecision {
    Allowed,
    Blocked {
        block: TmuxSendBlock,
        evidence: TmuxGuardBlockedEvidence,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BlockedTmuxSend {
    pub block: TmuxSendBlock,
    pub evidence: TmuxGuardBlockedEvidence,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ShellTmuxGuardError {
    message: String,
}

impl ShellTmuxGuardError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ShellTmuxGuardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for ShellTmuxGuardError {}

pub fn classify_shell_tmux_sends(
    script: &str,
    owned_panes: &BTreeSet<String>,
    current_pane: Option<&str>,
) -> Result<ShellTmuxGuardDecision, ShellTmuxGuardError> {
    inspect_shell_script(script, owned_panes, current_pane, 0)
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
    inspect_tmux_send_with_context(argv, owned_panes, current_pane)
        .unwrap_or(TmuxSendGuardDecision::Allowed)
}

pub fn inspect_tmux_send_with_context(
    argv: &[String],
    owned_panes: &BTreeSet<String>,
    current_pane: Option<&str>,
) -> Result<TmuxSendGuardDecision, ShellTmuxGuardError> {
    Ok(
        match inspect_blocked_tmux_send_with_context(argv, owned_panes, current_pane)? {
            Some(blocked) => TmuxSendGuardDecision::Blocked(blocked.block),
            None => TmuxSendGuardDecision::Allowed,
        },
    )
}

pub fn inspect_blocked_tmux_send_with_context(
    argv: &[String],
    owned_panes: &BTreeSet<String>,
    current_pane: Option<&str>,
) -> Result<Option<BlockedTmuxSend>, ShellTmuxGuardError> {
    let Some(command_index) = tmux_command_index(argv)? else {
        return Ok(None);
    };
    let command = argv[command_index].as_str();
    if !is_send_keys_command(command) {
        return Ok(None);
    }

    let send_args = &argv[command_index + 1..];
    let parsed = parse_options(send_args, &TMUX_SEND_OPTIONS)?;
    let target = match parsed.short_values.get(&'t').cloned() {
        Some(target) => target,
        None => match current_pane {
            Some(current_pane) => current_pane.to_string(),
            None => return Ok(None),
        },
    };
    if !owned_target_matches(&target, owned_panes) {
        return Ok(None);
    }

    let payload = &send_args[parsed.next_index..];
    let evidence = blocked_attempt_evidence(&parsed, &target, payload);
    Ok(Some(BlockedTmuxSend {
        block: TmuxSendBlock {
            reason: format!(
                "Direct tmux send to Humanize-owned pane {target} is blocked. Use Humanize MCP or the Humanize input tool so machine input is recorded."
            ),
            target,
        },
        evidence,
    }))
}

fn classify_normalized_tmux_send(
    argv: &[String],
    owned_panes: &BTreeSet<String>,
    current_pane: Option<&str>,
) -> Result<ShellTmuxGuardDecision, ShellTmuxGuardError> {
    Ok(
        match inspect_blocked_tmux_send_with_context(argv, owned_panes, current_pane)? {
            Some(blocked) => ShellTmuxGuardDecision::Blocked {
                block: blocked.block,
                evidence: blocked.evidence,
            },
            None => ShellTmuxGuardDecision::Allowed,
        },
    )
}

pub fn classify_tmux_send_from_runs_dir(
    argv: &[String],
    runs_dir: &Path,
    current_pane: Option<&str>,
) -> Result<TmuxSendGuardDecision, RunAssetError> {
    let owned_panes = discover_live_owned_tmux_panes_in_dir(runs_dir)?;
    Ok(classify_tmux_send_with_context(
        argv,
        &owned_targets(&owned_panes),
        current_pane,
    ))
}

pub fn owned_targets(owned_panes: &[OwnedTmuxPane]) -> BTreeSet<String> {
    owned_panes
        .iter()
        .flat_map(OwnedTmuxPane::targets)
        .collect::<BTreeSet<_>>()
}

pub fn owned_pane_matches_target(target: &str, owned: &OwnedTmuxPane) -> bool {
    owned_target_matches(target, &owned.targets().into_iter().collect())
}

fn inspect_shell_script(
    script: &str,
    owned_panes: &BTreeSet<String>,
    current_pane: Option<&str>,
    depth: usize,
) -> Result<ShellTmuxGuardDecision, ShellTmuxGuardError> {
    if depth > 8 {
        return Err(ShellTmuxGuardError::new(
            "shell command nesting exceeds the supported inspection depth",
        ));
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .map_err(|_| ShellTmuxGuardError::new("Bash parser could not be initialized"))?;
    let tree = parser
        .parse(script, None)
        .ok_or_else(|| ShellTmuxGuardError::new("Bash command could not be parsed"))?;
    if tree.root_node().has_error() {
        return Err(ShellTmuxGuardError::new(
            "Bash command contains unsupported or invalid syntax",
        ));
    }
    inspect_shell_node(
        tree.root_node(),
        script.as_bytes(),
        owned_panes,
        current_pane,
        depth,
    )
}

fn inspect_shell_node(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    owned_panes: &BTreeSet<String>,
    current_pane: Option<&str>,
    depth: usize,
) -> Result<ShellTmuxGuardDecision, ShellTmuxGuardError> {
    if node.kind() == "command"
        && let Some(decision) =
            inspect_command_node(node, source, owned_panes, current_pane, depth)?
    {
        return Ok(decision);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let decision = inspect_shell_node(child, source, owned_panes, current_pane, depth)?;
        if !matches!(decision, ShellTmuxGuardDecision::Allowed) {
            return Ok(decision);
        }
    }
    Ok(ShellTmuxGuardDecision::Allowed)
}

fn inspect_command_node(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    owned_panes: &BTreeSet<String>,
    current_pane: Option<&str>,
    depth: usize,
) -> Result<Option<ShellTmuxGuardDecision>, ShellTmuxGuardError> {
    let name_node = node
        .child_by_field_name("name")
        .ok_or_else(|| ShellTmuxGuardError::new("shell command name is missing"))?;
    let name = decode_literal_shell_word(node_text(name_node, source)?)?;
    if !is_normalizable_program(&name) {
        return Ok(None);
    }
    let argv = command_argv(node, source, &name)?;
    match normalize_command(&argv)? {
        NormalizedCommand::Tmux(tmux_argv) => {
            classify_normalized_tmux_send(tmux_argv, owned_panes, current_pane).map(Some)
        }
        NormalizedCommand::ShellScript(script) => {
            inspect_shell_script(&script, owned_panes, current_pane, depth + 1).map(Some)
        }
        NormalizedCommand::Other => Ok(None),
    }
}

fn command_argv(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    name: &str,
) -> Result<Vec<String>, ShellTmuxGuardError> {
    let mut argv = vec![name.to_string()];
    let mut cursor = node.walk();
    for argument in node.children_by_field_name("argument", &mut cursor) {
        argv.push(decode_literal_shell_word(node_text(argument, source)?)?);
    }
    Ok(argv)
}

fn node_text<'a>(
    node: tree_sitter::Node<'_>,
    source: &'a [u8],
) -> Result<&'a str, ShellTmuxGuardError> {
    node.utf8_text(source)
        .map_err(|_| ShellTmuxGuardError::new("shell command is not valid UTF-8"))
}

fn decode_literal_shell_word(word: &str) -> Result<String, ShellTmuxGuardError> {
    let mut decoded = String::new();
    let mut chars = word.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, '\'') => quote = Some('\''),
            (None, '"') => quote = Some('"'),
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), ch) => decoded.push(ch),
            (Some('"'), '\\') | (None, '\\') => {
                let next = chars
                    .next()
                    .ok_or_else(|| ShellTmuxGuardError::new("shell word ends with an escape"))?;
                decoded.push(next);
            }
            (Some('"'), '$' | '`') | (None, '$' | '`' | '*' | '?' | '[') => {
                return Err(ShellTmuxGuardError::new(
                    "dynamic shell words are outside the supported tmux inspection boundary",
                ));
            }
            (None, ch) if ch.is_whitespace() => {
                return Err(ShellTmuxGuardError::new(
                    "unquoted whitespace is not a literal shell word",
                ));
            }
            (_, ch) => decoded.push(ch),
        }
    }
    if quote.is_some() {
        return Err(ShellTmuxGuardError::new(
            "shell word has an unmatched quote",
        ));
    }
    Ok(decoded)
}

fn is_shell_program(value: &str) -> bool {
    matches!(program_name(value), Some("sh" | "bash" | "zsh"))
}

enum NormalizedCommand<'a> {
    Tmux(&'a [String]),
    ShellScript(String),
    Other,
}

fn normalize_command(argv: &[String]) -> Result<NormalizedCommand<'_>, ShellTmuxGuardError> {
    let mut current = argv;
    for _ in 0..8 {
        let Some(program) = current.first().and_then(|value| program_name(value)) else {
            return Ok(NormalizedCommand::Other);
        };
        if program == "tmux" {
            return Ok(NormalizedCommand::Tmux(current));
        }
        if program == "eval" {
            let parsed = parse_options(&current[1..], &EVAL_OPTIONS)?;
            let arguments = current.get(1 + parsed.next_index..).unwrap_or_default();
            return Ok(if arguments.is_empty() {
                NormalizedCommand::Other
            } else {
                NormalizedCommand::ShellScript(arguments.join(" "))
            });
        }
        if is_shell_program(program) {
            let parsed = parse_options(&current[1..], &SHELL_OPTIONS)?;
            return Ok(parsed
                .short_values
                .get(&'c')
                .cloned()
                .map_or(NormalizedCommand::Other, NormalizedCommand::ShellScript));
        }

        let next = match program {
            "env" => unwrap_env(current)?,
            "exec" => unwrap_options(current, &EXEC_OPTIONS)?,
            "command" => {
                let parsed = parse_options(&current[1..], &COMMAND_OPTIONS)?;
                if parsed.short_flags.contains(&'v') || parsed.short_flags.contains(&'V') {
                    return Ok(NormalizedCommand::Other);
                }
                current.get(1 + parsed.next_index..)
            }
            "time" => unwrap_options(current, &TIME_OPTIONS)?,
            "builtin" => {
                let parsed = parse_options(&current[1..], &BUILTIN_OPTIONS)?;
                let candidate = current.get(1 + parsed.next_index..);
                if candidate
                    .and_then(|args| args.first())
                    .and_then(|value| program_name(value))
                    .is_some_and(|name| matches!(name, "builtin" | "command" | "eval" | "exec"))
                {
                    candidate
                } else {
                    return Ok(NormalizedCommand::Other);
                }
            }
            _ => return Ok(NormalizedCommand::Other),
        };
        let Some(next) = next.filter(|args| !args.is_empty()) else {
            return Ok(NormalizedCommand::Other);
        };
        current = next;
    }
    Err(ShellTmuxGuardError::new(
        "shell command wrapper nesting exceeds the supported inspection depth",
    ))
}

fn is_normalizable_program(value: &str) -> bool {
    program_name(value).is_some_and(|name| {
        matches!(
            name,
            "bash"
                | "builtin"
                | "command"
                | "env"
                | "eval"
                | "exec"
                | "sh"
                | "time"
                | "tmux"
                | "zsh"
        )
    })
}

fn unwrap_env(argv: &[String]) -> Result<Option<&[String]>, ShellTmuxGuardError> {
    let parsed = parse_options(&argv[1..], &ENV_OPTIONS)?;
    let mut index = 1 + parsed.next_index;
    while argv
        .get(index)
        .is_some_and(|value| is_environment_assignment(value))
    {
        index += 1;
    }
    Ok(argv.get(index..))
}

fn unwrap_options<'a>(
    argv: &'a [String],
    grammar: &OptionGrammar,
) -> Result<Option<&'a [String]>, ShellTmuxGuardError> {
    let parsed = parse_options(&argv[1..], grammar)?;
    Ok(argv.get(1 + parsed.next_index..))
}

fn is_environment_assignment(value: &str) -> bool {
    value
        .split_once('=')
        .is_some_and(|(name, _)| !name.is_empty() && !name.contains('/'))
}

fn owned_target_matches(target: &str, owned_panes: &BTreeSet<String>) -> bool {
    let target_refs = public_target_refs(target);
    owned_panes.iter().any(|owned| {
        owned == target
            || target_refs.contains(owned)
            || pane_id_alias(owned).is_some_and(|pane_id| pane_id == target)
            || pane_id_alias(target).is_some_and(|pane_id| pane_id == owned)
            || target_is_parent_of_owned(target, owned)
    })
}

fn public_target_refs(target: &str) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    for value in [
        Some(target),
        pane_id_alias(target),
        window_target(target),
        session_target(target),
    ]
    .into_iter()
    .flatten()
    {
        refs.insert(public_hash_ref(value));
    }
    refs
}

fn public_hash_ref(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
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

fn tmux_command_index(argv: &[String]) -> Result<Option<usize>, ShellTmuxGuardError> {
    if !argv.first().is_some_and(|program| is_tmux_program(program)) {
        return Ok(None);
    }
    let parsed = parse_options(&argv[1..], &TMUX_GLOBAL_OPTIONS)?;
    let command_index = 1 + parsed.next_index;
    Ok((command_index < argv.len()).then_some(command_index))
}

fn is_tmux_program(value: &str) -> bool {
    program_name(value) == Some("tmux")
}

fn program_name(value: &str) -> Option<&str> {
    Path::new(value).file_name().and_then(|name| name.to_str())
}

struct OptionGrammar {
    short_flags: &'static str,
    short_values: &'static str,
    long_flags: &'static [&'static str],
    long_values: &'static [&'static str],
}

#[derive(Default)]
struct ParsedOptions {
    next_index: usize,
    short_flags: BTreeSet<char>,
    short_values: BTreeMap<char, String>,
}

fn parse_options(
    args: &[String],
    grammar: &OptionGrammar,
) -> Result<ParsedOptions, ShellTmuxGuardError> {
    let mut parsed = ParsedOptions::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--" {
            parsed.next_index = index + 1;
            return Ok(parsed);
        }
        if !arg.starts_with('-') || arg == "-" {
            parsed.next_index = index;
            return Ok(parsed);
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, attached) = long
                .split_once('=')
                .map_or((long, None), |(name, value)| (name, Some(value)));
            if grammar.long_flags.contains(&name) {
                if attached.is_some() {
                    return Err(ShellTmuxGuardError::new(format!(
                        "option --{name} does not take a value"
                    )));
                }
                index += 1;
                continue;
            }
            if grammar.long_values.contains(&name) {
                if attached.is_some_and(str::is_empty) {
                    return Err(ShellTmuxGuardError::new(format!(
                        "option --{name} requires a value"
                    )));
                }
                if attached.is_none() {
                    index += 1;
                    if index >= args.len() {
                        return Err(ShellTmuxGuardError::new(format!(
                            "option --{name} requires a value"
                        )));
                    }
                }
                index += 1;
                continue;
            }
            return Err(ShellTmuxGuardError::new(format!(
                "unsupported option --{name}"
            )));
        }

        let cluster = &arg[1..];
        if cluster.is_empty() {
            parsed.next_index = index;
            return Ok(parsed);
        }
        let mut chars = cluster.char_indices().peekable();
        while let Some((_, option)) = chars.next() {
            if grammar.short_flags.contains(option) {
                parsed.short_flags.insert(option);
                continue;
            }
            if grammar.short_values.contains(option) {
                let value_start = chars.peek().map(|(offset, _)| *offset);
                let value = if let Some(value_start) = value_start {
                    cluster[value_start..].trim_start_matches('=').to_string()
                } else {
                    index += 1;
                    args.get(index).cloned().ok_or_else(|| {
                        ShellTmuxGuardError::new(format!("option -{option} requires a value"))
                    })?
                };
                if value.is_empty() {
                    return Err(ShellTmuxGuardError::new(format!(
                        "option -{option} requires a value"
                    )));
                }
                parsed.short_values.insert(option, value);
                break;
            }
            return Err(ShellTmuxGuardError::new(format!(
                "unsupported option -{option}"
            )));
        }
        index += 1;
    }
    parsed.next_index = index;
    Ok(parsed)
}

fn blocked_attempt_evidence(
    parsed: &ParsedOptions,
    target: &str,
    payload: &[String],
) -> TmuxGuardBlockedEvidence {
    let operation = "send-keys".to_string();
    let option_flags = normalized_option_flags(parsed);
    let target_hash = hash_text(target);
    let payload_length: u64 = payload.iter().map(|value| value.len() as u64).sum();
    let payload_hash = hash_arguments(payload);

    let mut hasher = Sha256::new();
    hash_field(&mut hasher, &operation);
    for option in &option_flags {
        hash_field(&mut hasher, option);
    }
    hash_field(&mut hasher, &target_hash);
    hash_field(&mut hasher, &payload_hash);
    hasher.update(payload_length.to_be_bytes());
    let evidence_hash = format!("sha256:{:x}", hasher.finalize());

    TmuxGuardBlockedEvidence {
        operation,
        option_flags,
        target_hash,
        payload_length,
        payload_hash,
        evidence_hash,
    }
}

fn normalized_option_flags(parsed: &ParsedOptions) -> Vec<String> {
    parsed
        .short_flags
        .iter()
        .chain(parsed.short_values.keys())
        .map(|option| format!("-{option}"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, value);
    format!("sha256:{:x}", hasher.finalize())
}

fn hash_arguments(values: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((values.len() as u64).to_be_bytes());
    for value in values {
        hash_field(&mut hasher, value);
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

const TMUX_GLOBAL_OPTIONS: OptionGrammar = OptionGrammar {
    short_flags: "2CDhluNvV",
    short_values: "cfLST",
    long_flags: &[],
    long_values: &[],
};
const TMUX_SEND_OPTIONS: OptionGrammar = OptionGrammar {
    short_flags: "HKlMRX",
    short_values: "Nt",
    long_flags: &[],
    long_values: &[],
};
const EVAL_OPTIONS: OptionGrammar = OptionGrammar {
    short_flags: "",
    short_values: "",
    long_flags: &[],
    long_values: &[],
};
const ENV_OPTIONS: OptionGrammar = OptionGrammar {
    short_flags: "i0v",
    short_values: "uCa",
    long_flags: &[
        "ignore-environment",
        "null",
        "debug",
        "list-signal-handling",
    ],
    long_values: &["unset", "chdir", "argv0"],
};
const EXEC_OPTIONS: OptionGrammar = OptionGrammar {
    short_flags: "cl",
    short_values: "a",
    long_flags: &[],
    long_values: &[],
};
const COMMAND_OPTIONS: OptionGrammar = OptionGrammar {
    short_flags: "pvV",
    short_values: "",
    long_flags: &[],
    long_values: &[],
};
const BUILTIN_OPTIONS: OptionGrammar = OptionGrammar {
    short_flags: "",
    short_values: "",
    long_flags: &[],
    long_values: &[],
};
const TIME_OPTIONS: OptionGrammar = OptionGrammar {
    short_flags: "p",
    short_values: "",
    long_flags: &[],
    long_values: &[],
};
const SHELL_OPTIONS: OptionGrammar = OptionGrammar {
    short_flags: "abefhkmnptuvxBCEHPTlrsD",
    short_values: "coO",
    long_flags: &[
        "login",
        "noprofile",
        "norc",
        "posix",
        "restricted",
        "verbose",
    ],
    long_values: &[],
};
