use std::collections::BTreeSet;
use std::error::Error;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::str::FromStr;

use humanize_plugin::client_config::{ClientConfigTarget, render_client_config};
use humanize_plugin::mcp::{McpSurface, serve_stdio_signal_aware};
use humanize_plugin::pipe_sink::{
    PipeSinkAckRequest, PipeSinkIdentity, append_reader_to_pipe_log_under_root_with_completion,
};
use humanize_plugin::tmux_guard::{TmuxSendGuardDecision, classify_tmux_send_with_context};

const USAGE: &str = "usage: humanize-plugin-mcp [--list-tools|--version|--print-client-config <target> --command <path>|--guard-tmux-send [--owned-pane <target>...] [--current-pane <target>] -- <tmux args...>]";

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

fn run_pipe_sink(args: &[String]) -> Result<(), Box<dyn Error>> {
    let value = |flag: &str| -> Result<&str, Box<dyn Error>> {
        args.windows(2)
            .find_map(|window| (window[0] == flag).then_some(window[1].as_str()))
            .ok_or_else(|| format!("missing {flag}").into())
    };
    let root = Path::new(value("--root")?);
    let relative = Path::new(value("--relative")?);
    let ack_relative = Path::new(value("--ack-relative")?);
    let completion_relative = Path::new(value("--completion-relative")?);
    let ack_nonce = value("--ack-nonce")?;
    let identity = PipeSinkIdentity {
        dev: value("--dev")?.parse()?,
        ino: value("--ino")?.parse()?,
        uid: value("--uid")?.parse()?,
        mode: value("--mode")?.parse()?,
        nlink: value("--nlink")?.parse()?,
    };
    let ack = PipeSinkAckRequest::new(ack_relative, ack_nonce);
    let completion = PipeSinkAckRequest::new(completion_relative, ack_nonce);
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    append_reader_to_pipe_log_under_root_with_completion(
        root,
        relative,
        &identity,
        Some(&ack),
        Some(&completion),
        &mut reader,
    )?;
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

    match classify_tmux_send_with_context(
        &guard_tmux_argv(tmux_args),
        &owned_panes,
        current_pane.as_deref(),
    ) {
        TmuxSendGuardDecision::Allowed => Ok(()),
        TmuxSendGuardDecision::Blocked(block) => Err(block.reason.into()),
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
