mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::adapters::tmux::SystemCommandRunner;
use humanize_plugin::driver::DriverClient;
use humanize_plugin::mcp::{McpServer, TmuxExecutionDefaults};
use humanize_plugin::review::ReviewStore;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::json;

use support::driver_tmux::{ControlledTmuxFixture, fake_tmux_with_sequential_panes};
use support::mcp::{call_tool, lock_flow, structured};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn run_flow_binds_a_live_unbound_driver_without_creating_another_operator_pane() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut fixture = LiveUnboundDriverFixture::new("live-unbound-bind", "run-live-unbound");
    let initial_status = fixture
        .client()
        .request("status", fixture.run_id(), &json!({}));
    assert!(initial_status.unwrap()["run_mode"].is_null());

    let mut server = fixture.server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": fixture.run_id(),
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "run_mode": "continuous",
            "activation_limit": 3
        }),
    );
    assert!(fixture.tmux_control.wait_for_hooks());

    assert_eq!(structured(&started)["ok"], true, "{started}");
    assert_eq!(structured(&started)["run_mode"], "continuous");
    assert_eq!(structured(&started)["initial_activation_limit"], 3);
    assert!(
        structured(&started)["tmux"]["panes"][0]
            .get("pane_id")
            .is_none(),
        "{started}"
    );
    assert!(!started.to_string().contains("%9"), "{started}");

    let log = fs::read_to_string(fixture.root().join("tmux.log")).unwrap();
    assert_eq!(log.matches("split-window").count(), 1, "{log}");
    assert!(!log.contains("new-session"), "{log}");
    assert!(!log.contains("new-window"), "{log}");
    assert!(!log.contains("--run-id run-live-unbound"), "{log}");
    assert!(log.contains("split-window -P -F #{pane_id} -t host-a:%7"));

    fixture.shutdown();
}

#[test]
fn mcp_explicit_scheduling_rejects_quiescent_without_mutation() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut fixture = LiveUnboundDriverFixture::new(
        "quiescent-explicit-scheduling",
        "run-mcp-quiescent-explicit",
    );
    let mut server = fixture.server();
    let (lock_id, content_hash) = lock_flow(&mut server, 1, locked_agent_flow());
    let started = call_tool(
        &mut server,
        2,
        "run_flow",
        json!({
            "run_id": fixture.run_id(),
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "run_mode": "continuous",
            "activation_limit": 8
        }),
    );
    assert_eq!(structured(&started)["ok"], true, "{started}");
    assert!(fixture.tmux_control.wait_for_hooks());
    let delivered = call_tool(
        &mut server,
        3,
        "deliver_artifact",
        json!({
            "run_id": fixture.run_id(),
            "activation_id": "root",
            "artifact_key": "items",
            "payload": "alpha\nbeta"
        }),
    );
    assert_eq!(structured(&delivered)["ok"], true, "{delivered}");
    let stopped = fixture
        .client()
        .request(
            "observe_stop",
            fixture.run_id(),
            &json!({
                "activation_id": "root",
                "reason": "done"
            }),
        )
        .unwrap();
    assert_eq!(stopped["run_status"], "quiescent", "{stopped}");
    let before = call_tool(
        &mut server,
        5,
        "run_status",
        json!({ "run_id": fixture.run_id() }),
    );

    for (request_id, tool, arguments) in [
        (
            6,
            "activate_node",
            json!({
                "run_id": fixture.run_id(),
                "node_id": "manual"
            }),
        ),
        (
            7,
            "fanout_from_artifact",
            json!({
                "run_id": fixture.run_id(),
                "node_id": "batch",
                "artifact_key": "items",
                "for_each": "items"
            }),
        ),
    ] {
        let rejected = call_tool(&mut server, request_id, tool, arguments);
        assert_eq!(structured(&rejected)["ok"], false, "{rejected}");
        assert_eq!(
            structured(&rejected)["error"]["code"],
            "runtime_error",
            "{rejected}"
        );
        let after = call_tool(
            &mut server,
            request_id + 10,
            "run_status",
            json!({ "run_id": fixture.run_id() }),
        );
        assert_eq!(structured(&after)["run_status"], "quiescent", "{after}");
        assert_eq!(
            structured(&after)["event_cursor"],
            structured(&before)["event_cursor"],
            "{after}"
        );
        assert_eq!(
            structured(&after)["context_generation"],
            structured(&before)["context_generation"],
            "{after}"
        );
        assert_eq!(
            structured(&after)["context"]["activations"],
            structured(&before)["context"]["activations"],
            "{after}"
        );
    }
    fixture.shutdown();
}

struct LiveUnboundDriverFixture {
    root: PathBuf,
    run_id: String,
    child: Option<Child>,
    prior_tmux: Option<std::ffi::OsString>,
    prior_state_root: Option<std::ffi::OsString>,
    tmux_control: ControlledTmuxFixture,
}

impl LiveUnboundDriverFixture {
    fn new(name: &str, run_id: &str) -> Self {
        let root = std::env::temp_dir()
            .join("humanize-plugin-generation-mcp")
            .join(format!("{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(root.join("runs")).unwrap();
        fs::create_dir_all(root.join("runtime")).unwrap();
        fs::write(root.join("pane.counter"), "8\n").unwrap();
        let tmux_control = ControlledTmuxFixture::new(&root);
        let fake_tmux = fake_tmux_with_sequential_panes(&tmux_control);
        let prior_tmux = std::env::var_os("HUMANIZE_TMUX_BIN");
        let prior_state_root = std::env::var_os("HUMANIZE_STATE_ROOT");
        unsafe {
            std::env::set_var("HUMANIZE_TMUX_BIN", &fake_tmux);
            std::env::set_var("HUMANIZE_STATE_ROOT", &root);
        }

        let mut child = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-driver"))
            .arg("--run-id")
            .arg(run_id)
            .arg("--runs-root")
            .arg(root.join("runs"))
            .arg("--runtime-root")
            .arg(root.join("runtime"))
            .arg("--review-root")
            .arg(root.join("reviews"))
            .arg("--auth-token")
            .arg("test-token")
            .arg("--driver-session")
            .arg("host-a")
            .arg("--driver-window-id")
            .arg("%7")
            .arg("--driver-window-name")
            .arg("flow-a")
            .arg("--driver-pane-id")
            .arg("%8")
            .env("HUMANIZE_STATE_ROOT", &root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_for_driver(&root, run_id, &mut child);

        Self {
            root,
            run_id: run_id.to_string(),
            child: Some(child),
            prior_tmux,
            prior_state_root,
            tmux_control,
        }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn run_id(&self) -> &str {
        &self.run_id
    }

    fn run_root(&self) -> PathBuf {
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(self.root.join("runs")))
            .run_root(&self.run_id)
            .unwrap()
    }

    fn client(&self) -> DriverClient {
        DriverClient::from_run_root_for_run(&self.run_root(), &self.run_id)
            .unwrap()
            .unwrap()
    }

    fn server(&self) -> McpServer<SystemCommandRunner> {
        McpServer::with_tmux_runner_run_asset_store_and_execution_defaults(
            SystemCommandRunner,
            RunAssetStore::new(RunAssetSink::HumanizeRunsDir(self.root.join("runs"))),
            TmuxExecutionDefaults {
                session: Some("host-a".into()),
                window: Some("flow-a".into()),
                agent_command: Some("humanize-test-agent".into()),
            },
        )
        .with_review_store(ReviewStore::new(self.root.join("reviews")))
    }

    fn shutdown(&mut self) {
        assert!(self.tmux_control.wait_for_hooks());
        let response = self
            .client()
            .request("shutdown", &self.run_id, &json!({}))
            .unwrap();
        assert_eq!(response["ok"], true, "{response}");
        wait_for_exit(self.child.as_mut().unwrap());
        self.child = None;
    }
}

impl Drop for LiveUnboundDriverFixture {
    fn drop(&mut self) {
        let run_root = self.run_root();
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(client)) = DriverClient::from_run_root_for_run(&run_root, &self.run_id) {
                let _ = client.request("shutdown", &self.run_id, &json!({}));
            }
            if wait_for_exit_if_needed(child).is_err() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        unsafe {
            match self.prior_tmux.take() {
                Some(value) => std::env::set_var("HUMANIZE_TMUX_BIN", value),
                None => std::env::remove_var("HUMANIZE_TMUX_BIN"),
            }
            match self.prior_state_root.take() {
                Some(value) => std::env::set_var("HUMANIZE_STATE_ROOT", value),
                None => std::env::remove_var("HUMANIZE_STATE_ROOT"),
            }
        }
    }
}

fn wait_for_driver(root: &Path, run_id: &str, child: &mut Child) {
    let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(root.join("runs")));
    let run_root = store.run_root(run_id).unwrap();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if let Ok(Some(client)) = DriverClient::from_run_root_for_run(&run_root, run_id)
            && client
                .request("status", run_id, &json!({}))
                .is_ok_and(|status| status["ok"] == true)
        {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("driver exited before becoming ready: {status}");
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("driver did not become ready");
}

fn wait_for_exit(child: &mut Child) {
    wait_for_exit_if_needed(child).expect("driver did not shut down cleanly");
}

fn wait_for_exit_if_needed(child: &mut Child) -> Result<(), ()> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if child.try_wait().map_err(|_| ())?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(())
}

fn locked_agent_flow() -> serde_json::Value {
    json!({
        "nodes": [
            {
                "id": "root",
                "action": {
                    "driver": "agent",
                    "prompt_ref": "prompt.start",
                    "resource_refs": ["README.md"]
                }
            }
        ],
        "resources": [
            {
                "path": "README.md",
                "kind": "readme",
                "content": "Generation-aware driver bind fixture."
            },
            {
                "path": "prompt.start",
                "kind": "prompt",
                "content": "Inspect the repository."
            }
        ]
    })
}
