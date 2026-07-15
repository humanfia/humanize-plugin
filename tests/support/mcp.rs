// Each integration test crate compiles this support module independently.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::rc::Rc;
use std::time::Duration;

use humanize_plugin::adapters::tmux::{CommandOutput, CommandRunner, TmuxError};
use humanize_plugin::mcp::McpServer;
use serde_json::{Value, json};

thread_local! {
    static SIMULATED_PIPE_CAPTURES: RefCell<BTreeMap<String, SimulatedPipeCapture>> = const { RefCell::new(BTreeMap::new()) };
}

struct SimulatedPipeCapture {
    root: std::path::PathBuf,
    completion_relative: String,
    nonce: String,
    transcript_dev: u64,
    transcript_ino: u64,
    initial_len: u64,
    file: File,
}

#[derive(Clone, Default)]
pub struct RecordingRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    outputs: Rc<RefCell<VecDeque<CommandOutput>>>,
}
impl RecordingRunner {
    pub fn with_outputs(outputs: Vec<CommandOutput>) -> Self {
        Self {
            calls: Rc::new(RefCell::new(Vec::new())),
            outputs: Rc::new(RefCell::new(outputs.into())),
        }
    }

    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

#[derive(Clone)]
pub struct SideEffectRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    outputs: Rc<RefCell<VecDeque<CommandOutput>>>,
    on_call: RunnerSideEffect,
}

type RunnerSideEffect = Rc<dyn Fn(&[String])>;

impl SideEffectRunner {
    pub fn with_outputs(
        outputs: Vec<CommandOutput>,
        on_call: impl Fn(&[String]) + 'static,
    ) -> Self {
        Self {
            calls: Rc::new(RefCell::new(Vec::new())),
            outputs: Rc::new(RefCell::new(outputs.into())),
            on_call: Rc::new(on_call),
        }
    }

    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

impl CommandRunner for SideEffectRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        (self.on_call)(&argv);
        let output = self.outputs.borrow_mut().pop_front().unwrap_or_default();
        if argv.get(1).map(String::as_str) == Some("pipe-pane") && output.is_success() {
            acknowledge_pipe_command(&argv);
        }
        self.calls.borrow_mut().push(argv);
        Ok(output)
    }

    fn pipe_sink_helper_is_external(&self) -> bool {
        false
    }

    fn supports_external_driver_launch(&self) -> bool {
        false
    }

    fn pipe_sink_producer_closed(&self, target: &str) {
        complete_pipe_command(target);
    }
}
impl CommandRunner for RecordingRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        let output = self.outputs.borrow_mut().pop_front().unwrap_or_default();
        if argv.get(1).map(String::as_str) == Some("pipe-pane") && output.is_success() {
            acknowledge_pipe_command(&argv);
        }
        self.calls.borrow_mut().push(argv);
        Ok(output)
    }

    fn pipe_sink_helper_is_external(&self) -> bool {
        false
    }

    fn supports_external_driver_launch(&self) -> bool {
        false
    }

    fn pipe_sink_producer_closed(&self, target: &str) {
        complete_pipe_command(target);
    }
}

pub fn acknowledge_pipe_command(argv: &[String]) {
    let Some(command) = argv.get(5) else {
        return;
    };
    let Some(root) = shell_arg_after(command, "--root") else {
        return;
    };
    let Some(ack_relative) = shell_arg_after(command, "--ack-relative") else {
        return;
    };
    let Some(ack_nonce) = shell_arg_after(command, "--ack-nonce") else {
        return;
    };
    let Some(completion_relative) = shell_arg_after(command, "--completion-relative") else {
        return;
    };
    let Some(transcript_relative) = shell_arg_after(command, "--relative") else {
        return;
    };
    let Some(dev) = shell_arg_after(command, "--dev").and_then(|value| value.parse::<u64>().ok())
    else {
        return;
    };
    let Some(ino) = shell_arg_after(command, "--ino").and_then(|value| value.parse::<u64>().ok())
    else {
        return;
    };
    let transcript_path = std::path::Path::new(&root).join(transcript_relative);
    let Ok(file) = OpenOptions::new().append(true).open(&transcript_path) else {
        return;
    };
    let initial_len = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let ack_path = std::path::Path::new(&root).join(ack_relative);
    if let Some(parent) = ack_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let payload = json!({
        "nonce": ack_nonce,
        "pid": std::process::id(),
        "transcript_dev": dev,
        "transcript_ino": ino
    });
    let _ = fs::write(ack_path, format!("{payload}\n"));
    let target = argv.get(4).cloned().unwrap_or_default();
    SIMULATED_PIPE_CAPTURES.with(|captures| {
        captures.borrow_mut().insert(
            target,
            SimulatedPipeCapture {
                root: std::path::PathBuf::from(root),
                completion_relative,
                nonce: ack_nonce,
                transcript_dev: dev,
                transcript_ino: ino,
                initial_len,
                file,
            },
        );
    });
}

pub fn complete_pipe_command(target: &str) {
    let capture = SIMULATED_PIPE_CAPTURES.with(|captures| captures.borrow_mut().remove(target));
    let Some(capture) = capture else {
        return;
    };
    let _ = capture.file.sync_all();
    let transcript_len = capture
        .file
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(capture.initial_len);
    let completion_path = capture.root.join(capture.completion_relative);
    let payload = json!({
        "nonce": capture.nonce,
        "pid": std::process::id(),
        "process_start_time_ticks": current_process_start_time_ticks(),
        "transcript_dev": capture.transcript_dev,
        "transcript_ino": capture.transcript_ino,
        "initial_len": capture.initial_len,
        "bytes_appended": transcript_len.saturating_sub(capture.initial_len),
        "transcript_len": transcript_len
    });
    let _ = fs::write(completion_path, format!("{payload}\n"));
}

fn current_process_start_time_ticks() -> u64 {
    let stat = fs::read_to_string(format!("/proc/{}/stat", std::process::id())).unwrap();
    stat.rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap()
}

fn shell_arg_after(command: &str, flag: &str) -> Option<String> {
    let rest = command.split_once(flag)?.1.trim_start();
    if let Some(rest) = rest.strip_prefix('\'') {
        let value = rest.split('\'').next()?;
        return Some(value.to_string());
    }
    rest.split_whitespace().next().map(str::to_string)
}
pub fn call_tool<R: CommandRunner>(
    server: &mut McpServer<R>,
    id: u64,
    name: &str,
    mut arguments: Value,
) -> Value {
    if matches!(name, "run_flow" | "apply_flow_lock" | "apply_flow_update")
        && arguments.get("review_id").is_none()
        && arguments.get("reviewId").is_none()
    {
        let prepared = raw_call_tool(
            server,
            id.saturating_add(10_000),
            "prepare_flow_review",
            arguments.clone(),
        );
        if let Some(review_id) = structured(&prepared)["review_id"].as_str() {
            let decided = raw_call_tool(
                server,
                id.saturating_add(20_000),
                "decide_flow_review",
                json!({
                    "review_id": review_id,
                    "decision": "approved"
                }),
            );
            if structured(&decided)["ok"] == true {
                arguments["review_id"] = Value::String(review_id.to_string());
            }
        }
        if arguments.get("review_id").is_none() {
            arguments["review_id"] = Value::String("review-missing".to_string());
        }
    }
    raw_call_tool(server, id, name, arguments)
}

fn raw_call_tool<R: CommandRunner>(
    server: &mut McpServer<R>,
    id: u64,
    name: &str,
    arguments: Value,
) -> Value {
    server
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }))
        .expect("tool call should produce a response")
}
pub fn structured(response: &Value) -> &Value {
    &response["result"]["structuredContent"]
}
pub fn http_get(host: &str, port: u64, path: &str) -> String {
    let mut stream =
        TcpStream::connect((host, port as u16)).expect("browser view server should accept TCP");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout should be set");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
    )
    .expect("request should be written");

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("response should be readable");
    response
}
pub fn diagnostic_codes(response: &Value) -> Vec<&str> {
    structured(response)["diagnostics"]
        .as_array()
        .expect("diagnostics should be an array")
        .iter()
        .map(|diagnostic| {
            diagnostic["code"]
                .as_str()
                .expect("diagnostic should include a code")
        })
        .collect()
}
pub fn assert_tool_error(response: &Value) {
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(structured(response)["ok"], false);
}
pub fn assert_prefixed_hex(value: &str, prefix: &str) {
    let suffix = value
        .strip_prefix(prefix)
        .expect("value should include expected prefix");
    assert_eq!(suffix.len(), 64);
    assert!(suffix.chars().all(|ch| ch.is_ascii_hexdigit()));
}
pub fn readme_resource() -> Value {
    json!({
        "path": "README.md",
        "kind": "readme",
        "content": "Use Humanize to audit this library without editing files."
    })
}
pub fn missing_readme_flow() -> Value {
    json!({
        "nodes": ["root"]
    })
}
pub fn node_less_missing_readme_flow() -> Value {
    json!({
        "resources": [
            {
                "path": "schemas/handoff.json",
                "kind": "schema",
                "content": "handoff"
            }
        ]
    })
}
pub fn blank_inline_readme_flow() -> Value {
    json!({
        "nodes": ["root"],
        "resources": [
            {
                "path": "README.md",
                "kind": "readme",
                "content": "   "
            }
        ]
    })
}
pub fn valid_flow() -> Value {
    json!({
        "nodes": [
            { "id": "root" },
            { "id": "finish" }
        ],
        "resources": [readme_resource()],
        "routes": [
            {
                "predicate": {
                    "op": "exists",
                    "fact": {"kind": "artifact", "key": "ready"}
                },
                "activate": "finish"
            }
        ]
    })
}
pub fn lock_flow<R: CommandRunner>(
    server: &mut McpServer<R>,
    id: u64,
    flow: Value,
) -> (String, String) {
    let locked = call_tool(
        server,
        id,
        "flow_lock",
        json!({
            "flow": flow
        }),
    );

    assert_eq!(structured(&locked)["ok"], true);
    let lock_id = structured(&locked)["flow_lock_id"]
        .as_str()
        .expect("flow_lock should return a flow lock id")
        .to_string();
    let content_hash = structured(&locked)["content_hash"]
        .as_str()
        .expect("flow_lock should return a content hash")
        .to_string();

    (lock_id, content_hash)
}
pub fn lock_valid_flow<R: CommandRunner>(server: &mut McpServer<R>, id: u64) -> (String, String) {
    lock_flow(server, id, valid_flow())
}
pub fn populate_view_run<R: CommandRunner>(server: &mut McpServer<R>, run_id: &str) {
    let started = call_tool(
        server,
        1,
        "start_run",
        json!({
            "run_id": run_id,
            "nodes": [
                {
                    "id": "root",
                    "required_artifacts": ["brief", "report"],
                    "required_effects": ["shell", "review"]
                }
            ]
        }),
    );
    assert_eq!(structured(&started)["ok"], true);

    let artifact = call_tool(
        server,
        2,
        "deliver_artifact",
        json!({
            "run_id": run_id,
            "activation_id": "root",
            "artifact_key": "brief",
            "payload": "ready"
        }),
    );
    assert_eq!(structured(&artifact)["ok"], true);

    let effect = call_tool(
        server,
        3,
        "record_effect",
        json!({
            "run_id": run_id,
            "activation_id": "root",
            "effect_key": "shell",
            "payload": "cargo test"
        }),
    );
    assert_eq!(structured(&effect)["ok"], true);
}
