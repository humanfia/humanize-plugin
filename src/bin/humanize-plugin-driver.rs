use std::env;
use std::path::PathBuf;
use std::{fs, io};

use humanize_plugin::driver::{DriverConfig, DriverPaneConfig, run_driver};
use humanize_plugin::pipe_sink_cli::run_pipe_sink;

fn main() {
    if let Err(err) = try_main() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn try_main() -> Result<(), Box<dyn std::error::Error>> {
    let raw_args = env::args().skip(1).collect::<Vec<_>>();
    if matches!(raw_args.as_slice(), [arg] if arg == "--version") {
        println!("humanize-plugin-driver {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if raw_args.first().is_some_and(|arg| arg == "--pipe-sink") {
        return run_pipe_sink(&raw_args[1..]);
    }
    let args = DriverArgs::parse(raw_args)?;
    let auth_token = args.auth_token()?;
    Ok(run_driver(DriverConfig {
        run_id: args.run_id,
        runs_root: args.runs_root,
        runtime_root: args.runtime_root,
        auth_token,
        auth_token_path: args.auth_token_file,
        review_root: args.review_root,
        operator_pane: args.operator_pane,
    })?)
}

struct DriverArgs {
    run_id: String,
    runs_root: PathBuf,
    runtime_root: PathBuf,
    auth_token: Option<String>,
    auth_token_file: Option<PathBuf>,
    review_root: PathBuf,
    operator_pane: Option<DriverPaneConfig>,
}

impl DriverArgs {
    fn parse(args: impl IntoIterator<Item = String>) -> io::Result<Self> {
        let mut run_id = None;
        let mut runs_root = None;
        let mut runtime_root = None;
        let mut auth_token = None;
        let mut auth_token_file = None;
        let mut review_root = None;
        let mut driver_session = None;
        let mut driver_window_id = None;
        let mut driver_window_name = None;
        let mut driver_pane_id = None;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--run-id" => run_id = args.next(),
                "--runs-root" => runs_root = args.next().map(PathBuf::from),
                "--runtime-root" => runtime_root = args.next().map(PathBuf::from),
                "--auth-token" => auth_token = args.next(),
                "--auth-token-file" => auth_token_file = args.next().map(PathBuf::from),
                "--review-root" => review_root = args.next().map(PathBuf::from),
                "--driver-session" => driver_session = args.next(),
                "--driver-window-id" => driver_window_id = args.next(),
                "--driver-window-name" => driver_window_name = args.next(),
                "--driver-pane-id" => driver_pane_id = args.next(),
                "--help" | "-h" => {
                    println!(
                        "Usage: humanize-plugin-driver --run-id RUN --runs-root DIR --runtime-root DIR (--auth-token TOKEN | --auth-token-file FILE) [--review-root DIR] [--driver-session SESSION --driver-window-id WINDOW_ID --driver-window-name WINDOW --driver-pane-id PANE_ID]"
                    );
                    std::process::exit(0);
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown argument: {arg}"),
                    ));
                }
            }
        }
        let operator_pane = match (
            driver_session,
            driver_window_id,
            driver_window_name,
            driver_pane_id,
        ) {
            (None, None, None, None) => None,
            (Some(session_id), Some(window_id), Some(window_name), Some(pane_id)) => {
                Some(DriverPaneConfig {
                    session_id,
                    window_id,
                    window_name,
                    pane_id,
                })
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "all driver pane identity arguments are required together",
                ));
            }
        };
        let runs_root = require_arg(runs_root, "--runs-root")?;
        let review_root =
            review_root.unwrap_or_else(|| runs_root.parent().unwrap_or(&runs_root).join("reviews"));
        Ok(Self {
            run_id: require_arg(run_id, "--run-id")?,
            runs_root,
            runtime_root: require_arg(runtime_root, "--runtime-root")?,
            auth_token,
            auth_token_file,
            review_root,
            operator_pane,
        })
    }

    fn auth_token(&self) -> io::Result<String> {
        if let Some(token) = self.auth_token.as_ref() {
            return Ok(token.clone());
        }
        let path = self.auth_token_file.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "--auth-token or --auth-token-file is required",
            )
        })?;
        let token = fs::read_to_string(path)?;
        let token = token.trim();
        if token.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "auth token file is empty",
            ));
        }
        Ok(token.to_string())
    }
}

fn require_arg<T>(value: Option<T>, name: &str) -> io::Result<T> {
    value.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("{name} is required")))
}
