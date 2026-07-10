mod support;

use std::fs;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use humanize_plugin::adapters::tmux::CommandOutput;
use humanize_plugin::mcp::{McpServer, serve_stdio_signal_aware_with_server};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

use support::mcp::{RecordingRunner, call_tool, structured};

#[cfg(all(unix, target_os = "linux"))]
static SIGNAL_TEST_LOCK: Mutex<()> = Mutex::new(());

fn test_temp_dir(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(name);
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    path
}

#[test]
fn serve_stdio_finalizes_active_tmux_assets_on_eof() {
    let asset_root = test_temp_dir("mcp-run-assets-stdio-shutdown");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("shutdown capture\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "start_run",
            "arguments": {
                "run_id": "run-stdio-shutdown",
                "nodes": ["root"],
                "tmux": {
                    "enabled": true,
                    "session": "host-a",
                    "window": "flow-a"
                }
            }
        }
    })
    .to_string();
    let reader = signal_input(format!("{request}\n").as_bytes());

    let _writer = serve_stdio_signal_aware_with_server(&mut server, reader, Vec::new()).unwrap();

    let calls = runner.calls();
    let capture_index = first_tmux_call_index(&calls, "capture-pane");
    let kill_index = first_tmux_call_index(&calls, "kill-pane");
    assert!(capture_index < kill_index);
    let manifest_json = read_manifest(&asset_root, "run-stdio-shutdown");
    assert_eq!(
        manifest_json["activations"]["root"]["termination_reason"],
        "mcp_shutdown"
    );
    assert_eq!(
        manifest_json["activations"]["root"]["capture_complete"],
        true
    );
}

#[test]
fn serve_stdio_finalizes_active_tmux_assets_on_shutdown_request() {
    let asset_root = test_temp_dir("mcp-run-assets-stdio-shutdown-request");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("shutdown request capture\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let start_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "start_run",
            "arguments": {
                "run_id": "run-stdio-shutdown-request",
                "nodes": ["root"],
                "tmux": {
                    "enabled": true,
                    "session": "host-a",
                    "window": "flow-a"
                }
            }
        }
    })
    .to_string();
    let shutdown_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "shutdown"
    })
    .to_string();
    let ignored_request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/list"
    })
    .to_string();
    let reader = signal_input(
        format!("{start_request}\n{shutdown_request}\n{ignored_request}\n").as_bytes(),
    );

    let writer = serve_stdio_signal_aware_with_server(&mut server, reader, Vec::new()).unwrap();

    let responses = String::from_utf8(writer)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1]["result"]["ok"], true);
    assert_eq!(responses[1]["result"]["shutdown"], true);

    let calls = runner.calls();
    let capture_index = first_tmux_call_index(&calls, "capture-pane");
    let kill_index = first_tmux_call_index(&calls, "kill-pane");
    assert!(capture_index < kill_index);
    let manifest_json = read_manifest(&asset_root, "run-stdio-shutdown-request");
    assert_eq!(
        manifest_json["activations"]["root"]["termination_reason"],
        "mcp_shutdown"
    );
    assert_eq!(
        manifest_json["activations"]["root"]["capture_complete"],
        true
    );
}

#[test]
fn serve_stdio_finalizes_multiple_active_tmux_assets_on_eof() {
    let asset_root = test_temp_dir("mcp-run-assets-stdio-eof-multi");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("%9\n"),
        CommandOutput::success(""),
        CommandOutput::success("root eof capture\n"),
        CommandOutput::success(""),
        CommandOutput::success("reviewer eof capture\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "start_run",
            "arguments": {
                "run_id": "run-stdio-eof-multi",
                "nodes": ["root", "reviewer"],
                "tmux": {
                    "enabled": true,
                    "session": "host-a",
                    "window": "flow-a"
                }
            }
        }
    })
    .to_string();

    let _writer = serve_stdio_signal_aware_with_server(
        &mut server,
        signal_input(format!("{request}\n").as_bytes()),
        Vec::new(),
    )
    .unwrap();

    let calls = runner.calls();
    for target in ["host-a:%7.%8", "host-a:%7.%9"] {
        let capture_index = tmux_call_index(&calls, "capture-pane", target);
        let kill_index = tmux_call_index(&calls, "kill-pane", target);
        assert!(capture_index < kill_index);
    }
    let manifest_json = read_manifest(&asset_root, "run-stdio-eof-multi");
    assert_eq!(
        manifest_json["completion"]["complete_tmux_activations"],
        json!(["reviewer", "root"])
    );
}

#[test]
fn serve_stdio_finalizes_active_tmux_assets_on_read_error() {
    let asset_root = test_temp_dir("mcp-run-assets-stdio-read-error");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("read error capture\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-stdio-read-error",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let result = serve_stdio_signal_aware_with_server(&mut server, ErrorReader::new(), Vec::new());

    assert!(result.is_err());
    let calls = runner.calls();
    assert!(
        first_tmux_call_index(&calls, "capture-pane") < first_tmux_call_index(&calls, "kill-pane")
    );
}

#[test]
fn serve_stdio_finalizes_active_tmux_assets_on_broken_write() {
    let asset_root = test_temp_dir("mcp-run-assets-stdio-write-error");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("write error capture\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "start_run",
            "arguments": {
                "run_id": "run-stdio-write-error",
                "nodes": ["root"],
                "tmux": {
                    "enabled": true,
                    "session": "host-a",
                    "window": "flow-a"
                }
            }
        }
    })
    .to_string();

    let result = serve_stdio_signal_aware_with_server(
        &mut server,
        signal_input(format!("{request}\n").as_bytes()),
        BrokenWriter,
    );

    assert!(result.is_err());
    let calls = runner.calls();
    assert!(
        first_tmux_call_index(&calls, "capture-pane") < first_tmux_call_index(&calls, "kill-pane")
    );
}

#[test]
fn serve_stdio_reports_shutdown_failure_when_a_tmux_pane_remains() {
    let asset_root = test_temp_dir("mcp-run-assets-stdio-kill-failure");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("shutdown capture\n"),
        CommandOutput::failure("kill failed"),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "start_run",
            "arguments": {
                "run_id": "run-stdio-kill-failure",
                "nodes": ["root"],
                "tmux": {
                    "enabled": true,
                    "session": "host-a",
                    "window": "flow-a"
                }
            }
        }
    })
    .to_string();

    let result = serve_stdio_signal_aware_with_server(
        &mut server,
        signal_input(format!("{request}\n").as_bytes()),
        Vec::new(),
    );

    let error = result.expect_err("remaining tmux pane must fail orderly shutdown");
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(error.to_string().contains("tmux resource cleanup failed"));
    let calls = runner.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("capture-pane"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("kill-pane"))
            .count(),
        1
    );
    let manifest = read_manifest(&asset_root, "run-stdio-kill-failure");
    assert_eq!(manifest["activations"]["root"]["capture_complete"], false);
    assert_eq!(
        manifest["activations"]["root"]["preservation_status"],
        "capturing"
    );
    assert_eq!(
        manifest["activations"]["root"]["resource_cleanup_status"],
        "failed"
    );
}

#[test]
fn shutdown_reports_incomplete_expected_activation_after_pane_cleanup() {
    let asset_root = test_temp_dir("mcp-run-assets-shutdown-manifest-omission");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::failure("pipe setup failed"),
        CommandOutput::success("final pane capture\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner, store);
    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-shutdown-manifest-omission",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(started["result"]["isError"], true);

    let shutdown = server
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "shutdown"
        }))
        .unwrap();

    assert_eq!(shutdown["result"]["ok"], false);
    assert_eq!(shutdown["result"]["shutdown"], true);
    let manifest = read_manifest(&asset_root, "run-shutdown-manifest-omission");
    assert_eq!(manifest["preservation_blocked"], true);
    assert_eq!(
        manifest["completion"]["incomplete_tmux_activations"],
        json!(["root"])
    );
}

#[test]
fn shutdown_response_reports_cleanup_errors_without_repeating_finalization() {
    let asset_root = test_temp_dir("mcp-run-assets-shutdown-response-failure");
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("shutdown capture\n"),
        CommandOutput::failure("kill failed"),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": "run-shutdown-response-failure",
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let first = server
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "shutdown"
        }))
        .unwrap();
    let second = server
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "shutdown"
        }))
        .unwrap();

    for response in [&first, &second] {
        assert_eq!(response["result"]["ok"], false);
        assert_eq!(response["result"]["shutdown"], true);
        assert_eq!(response["result"]["tmux_cleanup"]["remaining_panes"], 1);
        assert_eq!(
            response["result"]["tmux_cleanup"]["runs"][0]["cleanup_errors"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    let calls = runner.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("capture-pane"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).map(String::as_str) == Some("kill-pane"))
            .count(),
        1
    );
}

#[cfg(all(unix, target_os = "linux"))]
#[test]
fn signal_aware_stdio_releases_handlers_and_workers_after_eof() {
    let _guard = SIGNAL_TEST_LOCK.lock().unwrap();
    let before = linux_process_signal_state();
    let mut server = McpServer::with_tmux_runner(RecordingRunner::default());

    let result = serve_stdio_signal_aware_with_server(&mut server, signal_input(&[]), Vec::new());
    assert!(result.is_ok());
    thread::sleep(Duration::from_millis(50));
    assert_signal_state_released(&before, &linux_process_signal_state());

    let mut later_server = McpServer::with_tmux_runner(RecordingRunner::default());
    let later_result =
        serve_stdio_signal_aware_with_server(&mut later_server, signal_input(&[]), Vec::new());
    assert!(later_result.is_ok());
    thread::sleep(Duration::from_millis(50));

    assert_signal_state_released(&before, &linux_process_signal_state());
}

#[cfg(unix)]
#[test]
fn serve_stdio_finalizes_active_tmux_assets_on_sighup_while_reader_is_blocked() {
    assert_signal_shutdown_finalizes_assets(libc::SIGHUP, "run-stdio-sighup");
}

#[cfg(unix)]
#[test]
fn serve_stdio_finalizes_active_tmux_assets_on_sigterm_while_reader_is_blocked() {
    assert_signal_shutdown_finalizes_assets(libc::SIGTERM, "run-stdio-sigterm");
}

#[cfg(unix)]
#[test]
fn serve_stdio_finalizes_active_tmux_assets_on_sigint_while_reader_is_blocked() {
    assert_signal_shutdown_finalizes_assets(libc::SIGINT, "run-stdio-sigint");
}

fn read_manifest(root: &Path, run_id: &str) -> Value {
    read_json(find_manifest_path(root, run_id))
}

fn find_manifest_path(root: &Path, run_id: &str) -> PathBuf {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let manifest_path = entry.path().join("manifest.json");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = read_json(&manifest_path);
        if manifest["run_id"] == run_id {
            return manifest_path;
        }
    }
    panic!(
        "manifest for {run_id} should exist below {}",
        root.display()
    );
}

fn read_json(path: impl AsRef<Path>) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn first_tmux_call_index(calls: &[Vec<String>], command: &str) -> usize {
    calls
        .iter()
        .position(|call| call.get(1).map(String::as_str) == Some(command))
        .unwrap_or_else(|| panic!("{command} should be called"))
}

fn tmux_call_index(calls: &[Vec<String>], command: &str, target: &str) -> usize {
    calls
        .iter()
        .position(|call| {
            call.get(1).map(String::as_str) == Some(command) && call.iter().any(|arg| arg == target)
        })
        .unwrap_or_else(|| panic!("{command} should target {target}"))
}

struct ErrorReader {
    read: UnixStream,
}

impl ErrorReader {
    fn new() -> Self {
        let (read, mut write) = UnixStream::pair().unwrap();
        write.write_all(b"x").unwrap();
        write.shutdown(Shutdown::Write).unwrap();
        Self { read }
    }
}

impl Read for ErrorReader {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("read failed"))
    }
}

impl AsRawFd for ErrorReader {
    fn as_raw_fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }
}

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
fn assert_signal_shutdown_finalizes_assets(signal: libc::c_int, run_id: &str) {
    #[cfg(target_os = "linux")]
    let _guard = SIGNAL_TEST_LOCK.lock().unwrap();
    let asset_root = test_temp_dir(&format!("mcp-run-assets-{run_id}"));
    let runner = RecordingRunner::with_outputs(vec![
        CommandOutput::failure("missing session"),
        CommandOutput::success("%7\t%8\n"),
        CommandOutput::success(""),
        CommandOutput::success("signal capture\n"),
        CommandOutput::success(""),
    ]);
    let store = RunAssetStore::new(RunAssetSink::Root(asset_root.clone()));
    let mut server = McpServer::with_tmux_runner_and_run_asset_store(runner.clone(), store);
    let started = call_tool(
        &mut server,
        1,
        "start_run",
        json!({
            "run_id": run_id,
            "nodes": ["root"],
            "tmux": {
                "enabled": true,
                "session": "host-a",
                "window": "flow-a"
            }
        }),
    );
    assert_eq!(structured(&started)["ok"], true);
    let (reader, peer) = UnixStream::pair().unwrap();
    let signal_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        unsafe {
            libc::raise(signal);
        }
        drop(peer);
    });

    let result = serve_stdio_signal_aware_with_server(&mut server, reader, Vec::new());
    signal_thread.join().unwrap();

    assert!(result.is_ok());
    let calls = runner.calls();
    assert!(
        first_tmux_call_index(&calls, "capture-pane") < first_tmux_call_index(&calls, "kill-pane")
    );
    let manifest_json = read_manifest(&asset_root, run_id);
    assert_eq!(
        manifest_json["activations"]["root"]["termination_reason"],
        "mcp_shutdown"
    );
    assert_eq!(
        manifest_json["activations"]["root"]["capture_complete"],
        true
    );
}

fn signal_input(bytes: &[u8]) -> UnixStream {
    let (read, mut write) = UnixStream::pair().unwrap();
    write.write_all(bytes).unwrap();
    write.shutdown(Shutdown::Write).unwrap();
    read
}

#[cfg(all(unix, target_os = "linux"))]
fn linux_process_signal_state() -> (String, usize) {
    let status = fs::read_to_string("/proc/self/status").unwrap();
    let signal_handlers = status
        .lines()
        .find(|line| line.starts_with("SigCgt:"))
        .unwrap()
        .to_string();
    let threads = status
        .lines()
        .find(|line| line.starts_with("Threads:"))
        .unwrap()
        .split_once(':')
        .unwrap()
        .1
        .trim()
        .parse()
        .unwrap();
    (signal_handlers, threads)
}

#[cfg(all(unix, target_os = "linux"))]
fn assert_signal_state_released(before: &(String, usize), after: &(String, usize)) {
    assert_eq!(after.0, before.0);
    assert!(
        after.1 <= before.1,
        "signal-aware stdio left an additional worker thread: before {}, after {}",
        before.1,
        after.1
    );
}
