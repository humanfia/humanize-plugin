use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

#[test]
fn participant_stdio_lists_only_scoped_tools_and_rejects_operator_calls() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("mcp-participant-binding-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let binding_path = root.join("binding.json");
    fs::write(
        &binding_path,
        serde_json::to_vec_pretty(&json!({
            "protocol":"humanize.participant_binding.v1",
            "run_id":"run-participant",
            "activation_id":"review",
            "allocation_generation":0,
            "pane_id":"%8",
            "readiness_nonce":"ready",
            "handle":"participant-handle",
            "credential":"participant-credential",
            "runs_root":root.join("runs")
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&binding_path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-mcp"))
        .env("HUMANIZE_PARTICIPANT_BINDING_FILE", &binding_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        for request in [
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"tools/call",
                "params":{"name":"flow_check","arguments":{"flow":{}}}
            }),
            json!({
                "jsonrpc":"2.0",
                "id":3,
                "method":"tools/call",
                "params":{
                    "name":"deliver_artifact",
                    "arguments":{"artifact_key":"report","payload":"ready"}
                }
            }),
            json!({
                "jsonrpc":"2.0",
                "id":4,
                "method":"tools/call",
                "params":{
                    "name":"record_effect",
                    "arguments":{
                        "run_id":"other-run",
                        "activation_id":"other-activation",
                        "effect_key":"tests",
                        "payload":"passed"
                    }
                }
            }),
        ] {
            writeln!(stdin, "{request}").unwrap();
        }
    }

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "get_context",
            "deliver_artifact",
            "record_effect",
            "validate_stop"
        ]
    );
    for tool in tools {
        let schema = &tool["inputSchema"];
        assert!(schema["properties"].get("run_id").is_none(), "{tool}");
        assert!(
            schema["properties"].get("activation_id").is_none(),
            "{tool}"
        );
    }
    assert_eq!(responses[1]["error"]["message"], "unknown tool");
    assert_eq!(
        responses[2]["result"]["structuredContent"]["error"]["code"],
        "driver_authority_required"
    );
    assert!(
        responses[3]["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("participant binding")),
        "{}",
        responses[3]
    );
    fs::remove_dir_all(root).unwrap();
}
