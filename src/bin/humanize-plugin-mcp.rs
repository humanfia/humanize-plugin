use std::error::Error;
use std::io::{self, BufReader, BufWriter, Write};
use std::str::FromStr;

use humanize_plugin::client_config::{ClientConfigTarget, render_client_config};
use humanize_plugin::mcp::{McpSurface, serve_stdio};

const USAGE: &str = "usage: humanize-plugin-mcp [--list-tools|--version|--print-client-config <target> --command <path>]";

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
            let mut reader = BufReader::new(stdin.lock());
            let mut writer = BufWriter::new(stdout.lock());
            serve_stdio(&mut reader, &mut writer)?;
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
