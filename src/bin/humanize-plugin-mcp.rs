use std::error::Error;
use std::io::{self, BufReader, BufWriter, Write};

use humanize_plugin::mcp::{McpSurface, serve_stdio};

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
        _ => {
            return Err("usage: humanize-plugin-mcp [--list-tools|--version]".into());
        }
    }

    Ok(())
}
