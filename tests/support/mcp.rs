// Each integration test crate compiles this support module independently.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::rc::Rc;
use std::time::Duration;

use humanize_plugin::adapters::tmux::{CommandOutput, CommandRunner, TmuxError};
use humanize_plugin::mcp::McpServer;
use serde_json::{Value, json};

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
impl CommandRunner for RecordingRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        self.calls.borrow_mut().push(argv);
        Ok(self.outputs.borrow_mut().pop_front().unwrap_or_default())
    }
}
pub fn call_tool<R: CommandRunner>(
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
    assert_eq!(suffix.len(), 16);
    assert!(suffix.chars().all(|ch| ch.is_ascii_hexdigit()));
}
pub fn readme_resource() -> Value {
    json!({
        "id": "readme.main",
        "kind": "readme",
        "source": "inline:Use Humanize to audit this library without editing files."
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
                "id": "schema.handoff",
                "kind": "schema",
                "source": "inline:handoff"
            }
        ]
    })
}
pub fn blank_inline_readme_flow() -> Value {
    json!({
        "nodes": ["root"],
        "resources": [
            {
                "id": "readme.main",
                "kind": "readme",
                "source": "inline:   "
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
                "predicate": "exists(artifact.ready)",
                "activate": "finish"
            }
        ]
    })
}
pub fn lock_valid_flow<R: CommandRunner>(server: &mut McpServer<R>, id: u64) -> (String, String) {
    let locked = call_tool(
        server,
        id,
        "flow_lock",
        json!({
            "flow": valid_flow()
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
