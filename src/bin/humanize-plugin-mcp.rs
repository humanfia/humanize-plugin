use std::collections::BTreeSet;
use std::error::Error;
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::process::Command;
use std::str::FromStr;

use humanize_plugin::client_config::{
    ClientConfigTarget, render_client_config, run_participant_exited_hook, run_session_start_hook,
    run_stop_hook,
};
use humanize_plugin::driver::DriverClient;
use humanize_plugin::mcp::{McpSurface, serve_stdio_signal_aware};
use humanize_plugin::pipe_sink_cli::run_pipe_sink;
use humanize_plugin::run_assets::{
    OwnedTmuxPane, RunAssetStore, TMUX_GUARD_BLOCKED_HOOK, TmuxGuardBlockedEvidence,
};
use humanize_plugin::tmux_guard::{
    BlockedTmuxSend, ShellTmuxGuardDecision, TmuxSendGuardDecision, classify_shell_tmux_sends,
    inspect_blocked_tmux_send_with_context, inspect_tmux_send_with_context,
    owned_pane_matches_target, owned_targets,
};
use serde_json::{Value, json};

const USAGE: &str = "usage: humanize-plugin-mcp [--list-tools|--version|--print-client-config <target> --command <path>|--agent-ready-hook [--source <name>]|--codex-pre-tool-use-hook|--claude-pre-tool-use-hook|--codex-stop-hook|--claude-stop-hook|--participant-exited-hook --exit-status <code>|--guarded-tmux -- <tmux args...>|--guard-tmux-send [--owned-pane <target>...] [--current-pane <target>] -- <tmux args...>]";
const NATIVE_HOOK_PROTOCOL_DENIAL: &str = "The native hook request did not match the supported Humanize PreToolUse protocol. The command remains denied; repair the hook integration before retrying.";

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [] => {
            let stdin = io::stdin();
            let stdout = io::stdout();
            let reader = stdin.lock();
            let writer = BufWriter::new(stdout);
            serve_stdio_signal_aware(reader, writer)?;
        }
        [flag, pipe_args @ ..] if flag == "--pipe-sink" => {
            run_pipe_sink(pipe_args)?;
        }
        [flag] if flag == "--list-tools" => {
            let stdout = io::stdout();
            let mut writer = BufWriter::new(stdout.lock());
            serde_json::to_writer_pretty(&mut writer, &McpSurface.tools_list_json())?;
            writeln!(writer)?;
        }
        [flag] if flag == "--version" => {
            println!("humanize-plugin-mcp {}", env!("CARGO_PKG_VERSION"));
        }
        [flag, target, command_flag, command]
            if flag == "--print-client-config" && command_flag == "--command" =>
        {
            let target = ClientConfigTarget::from_str(target).map_err(client_config_usage_error)?;
            let rendered =
                render_client_config(target, command).map_err(client_config_usage_error)?;
            println!("{rendered}");
        }
        [flag, guard_args @ ..] if flag == "--guard-tmux-send" => {
            guard_tmux_send(guard_args)?;
        }
        [flag, ready_args @ ..] if flag == "--agent-ready-hook" => {
            agent_ready_hook(ready_args, &mut io::stdin().lock())?;
        }
        [flag] if flag == "--codex-pre-tool-use-hook" || flag == "--claude-pre-tool-use-hook" => {
            native_pre_tool_use_hook();
        }
        [flag] if flag == "--codex-stop-hook" || flag == "--claude-stop-hook" => {
            let platform = if flag == "--codex-stop-hook" {
                "codex"
            } else {
                "claude"
            };
            let response = run_stop_hook(platform, &mut io::stdin().lock())?;
            println!("{response}");
        }
        [flag, exit_flag, exit_status]
            if flag == "--participant-exited-hook" && exit_flag == "--exit-status" =>
        {
            let exit_status = exit_status
                .parse::<i32>()
                .map_err(|_| guard_usage_error("invalid participant exit status"))?;
            run_participant_exited_hook(exit_status)?;
        }
        [flag, tmux_args @ ..] if flag == "--guarded-tmux" => {
            guarded_tmux(tmux_args)?;
        }
        [flag, ..] if flag == "--print-client-config" => {
            return Err(client_config_usage_error(
                "missing client config target or command",
            ));
        }
        _ => {
            return Err(USAGE.into());
        }
    }

    Ok(())
}

fn client_config_usage_error(error: impl ToString) -> Box<dyn Error> {
    format!("{}\n{USAGE}", error.to_string()).into()
}

fn guard_tmux_send(args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut owned_panes = BTreeSet::new();
    let mut current_pane = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--owned-pane" => {
                let Some(target) = args.get(index + 1) else {
                    return Err(guard_usage_error("missing owned pane target"));
                };
                owned_panes.insert(target.clone());
                index += 2;
            }
            "--current-pane" => {
                let Some(target) = args.get(index + 1) else {
                    return Err(guard_usage_error("missing current pane target"));
                };
                current_pane = Some(target.clone());
                index += 2;
            }
            "--" => {
                index += 1;
                break;
            }
            _ => {
                return Err(guard_usage_error(format!(
                    "unknown guard argument: {}",
                    args[index]
                )));
            }
        }
    }

    let tmux_args = &args[index..];
    if tmux_args.is_empty() {
        return Err(guard_usage_error("missing tmux arguments"));
    }
    if owned_panes.is_empty() {
        let store = RunAssetStore::runtime_default();
        owned_panes = owned_targets(&store.discover_live_owned_tmux_panes()?);
    }

    match inspect_tmux_send_with_context(
        &guard_tmux_argv(tmux_args),
        &owned_panes,
        current_pane.as_deref(),
    )? {
        TmuxSendGuardDecision::Allowed => Ok(()),
        TmuxSendGuardDecision::Blocked(block) => Err(block.reason.into()),
    }
}

fn agent_ready_hook(args: &[String], reader: &mut impl Read) -> Result<(), Box<dyn Error>> {
    let mut source = "session_start".to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(guard_usage_error("missing ready source"));
                };
                source = value.clone();
                index += 2;
            }
            other => {
                return Err(guard_usage_error(format!(
                    "unknown ready argument: {other}"
                )));
            }
        }
    }
    run_session_start_hook(&source, reader)?;
    Ok(())
}

fn native_pre_tool_use_hook() {
    let command = match read_native_hook_command(&mut io::stdin().lock()) {
        Ok(command) => command,
        Err(()) => {
            print_hook_denial(NATIVE_HOOK_PROTOCOL_DENIAL);
            return;
        }
    };
    let store = RunAssetStore::runtime_default();
    let owned_panes = match store.discover_live_owned_tmux_panes() {
        Ok(owned_panes) => owned_panes,
        Err(_) => {
            print_hook_denial(
                "Humanize tmux ownership could not be verified. Route model control through MCP and repair the durable run metadata.",
            );
            return;
        }
    };
    let current_pane = std::env::var("TMUX_PANE").ok();
    match classify_shell_tmux_sends(
        &command,
        &owned_targets(&owned_panes),
        current_pane.as_deref(),
    ) {
        Ok(ShellTmuxGuardDecision::Allowed) => {}
        Ok(ShellTmuxGuardDecision::Blocked { block, evidence }) => {
            let Some(owner) = owned_panes
                .iter()
                .find(|owned| owned_pane_matches_target(&block.target, owned))
            else {
                print_hook_denial(
                    "Humanize tmux ownership could not be resolved safely. Route model control through MCP and repair the durable run metadata.",
                );
                return;
            };
            match record_blocked_attempt(owner, &evidence) {
                Ok(()) => print_hook_denial(&block.reason),
                Err(_) => print_hook_denial(
                    "The blocked Humanize tmux input attempt could not be durably recorded. The command remains denied; route model control through MCP and repair run storage.",
                ),
            }
        }
        Err(_) => print_hook_denial(
            "The shell command could not be safely inspected for tmux input. Use literal supported shell syntax and route Humanize pane control through MCP.",
        ),
    }
}

fn read_native_hook_command(reader: &mut impl Read) -> Result<String, ()> {
    let mut input = String::new();
    reader.read_to_string(&mut input).map_err(|_| ())?;
    let event = serde_json::from_str::<Value>(&input).map_err(|_| ())?;
    let event = event.as_object().ok_or(())?;
    if event.get("hook_event_name").and_then(Value::as_str) != Some("PreToolUse") {
        return Err(());
    }
    if event.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return Err(());
    }
    event
        .get("tool_input")
        .and_then(Value::as_object)
        .and_then(|input| input.get("command"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or(())
}

fn print_hook_denial(reason: &str) {
    let stdout = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }
    });
    println!("{stdout}");
}

fn guarded_tmux(args: &[String]) -> Result<(), Box<dyn Error>> {
    let tmux_args = strip_double_dash(args);
    if tmux_args.is_empty() {
        return Err(guard_usage_error("missing tmux arguments"));
    }
    let argv = guard_tmux_argv(tmux_args);
    if let Some((owner, blocked)) = discovered_blocked_send(&argv)? {
        record_blocked_attempt(&owner, &blocked.evidence)?;
        return Err(blocked.block.reason.into());
    }
    let real_tmux = std::env::var_os("HUMANIZE_TMUX_BIN")
        .filter(|value| !value.is_empty())
        .ok_or("HUMANIZE_TMUX_BIN is required for guarded tmux wrapper")?;
    let status = Command::new(real_tmux).args(tmux_args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tmux exited with status {}", status.code().unwrap_or(1)).into())
    }
}

fn discovered_blocked_send(
    argv: &[String],
) -> Result<Option<(OwnedTmuxPane, BlockedTmuxSend)>, Box<dyn Error>> {
    let store = RunAssetStore::runtime_default();
    let owned_panes = store.discover_live_owned_tmux_panes()?;
    let current_pane = std::env::var("TMUX_PANE").ok();
    match inspect_blocked_tmux_send_with_context(
        argv,
        &owned_targets(&owned_panes),
        current_pane.as_deref(),
    )? {
        None => Ok(None),
        Some(blocked) => {
            let owner = owned_panes
                .into_iter()
                .find(|owned| owned_pane_matches_target(&blocked.block.target, owned))
                .ok_or("blocked tmux target did not resolve to an owned pane")?;
            Ok(Some((owner, blocked)))
        }
    }
}

fn record_blocked_attempt(
    owner: &OwnedTmuxPane,
    evidence: &TmuxGuardBlockedEvidence,
) -> Result<(), Box<dyn Error>> {
    let run_root = owner
        .manifest_path
        .parent()
        .ok_or("owned pane public run root is unavailable")?;
    let client = DriverClient::from_run_root_for_run(run_root, &owner.run_id)?
        .ok_or("runtime driver is unavailable")?;
    let mut arguments = json!({
        "session_id": owner.session_id,
        "hook": TMUX_GUARD_BLOCKED_HOOK,
        "source_native_id": format!("tmux_guard_blocked:{}", evidence.evidence_hash),
        "payload": {
            "decision": "blocked",
            "operation": evidence.operation,
            "option_flags": evidence.option_flags,
            "target_hash": evidence.target_hash,
            "payload_hash": evidence.payload_hash,
            "payload_length": evidence.payload_length
        }
    });
    if owner.activation_id != "driver" {
        arguments["activation_id"] = Value::String(owner.activation_id.clone());
    }
    let response = client.request("record_hook_fact", &owner.run_id, &arguments)?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = response
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("runtime driver rejected tmux guard evidence");
        return Err(message.to_string().into());
    }
    Ok(())
}

fn strip_double_dash(args: &[String]) -> &[String] {
    if args.first().is_some_and(|arg| arg == "--") {
        &args[1..]
    } else {
        args
    }
}

fn guard_tmux_argv(tmux_args: &[String]) -> Vec<String> {
    if tmux_args.first().is_some_and(|arg| is_tmux_program(arg)) {
        return tmux_args.to_vec();
    }

    let mut argv = Vec::with_capacity(tmux_args.len() + 1);
    argv.push("tmux".to_string());
    argv.extend_from_slice(tmux_args);
    argv
}

fn is_tmux_program(value: &str) -> bool {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "tmux")
}

fn guard_usage_error(error: impl ToString) -> Box<dyn Error> {
    format!("{}\n{USAGE}", error.to_string()).into()
}
