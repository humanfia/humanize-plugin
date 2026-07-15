mod support;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::mcp::McpServer;
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::json;

use support::mcp::{RecordingRunner, call_tool, structured, valid_flow};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

fn root(name: &str) -> PathBuf {
    let index = NEXT_ROOT.fetch_add(1, Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("mcp-flow-package-{name}-{index}"));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    root
}

fn server(asset_root: PathBuf) -> McpServer<RecordingRunner> {
    McpServer::with_tmux_runner_and_run_asset_store(
        RecordingRunner::default(),
        RunAssetStore::new(RunAssetSink::Root(asset_root)),
    )
}

#[test]
fn fresh_mcp_process_state_loads_an_explicit_directory_package() {
    let root = root("reload");
    let package = root.join("package with space");
    let mut first = server(root.join("first-assets"));

    let locked = call_tool(
        &mut first,
        1,
        "flow_lock",
        json!({
            "flow": valid_flow(),
            "package_path": package
        }),
    );

    assert_eq!(structured(&locked)["ok"], true, "{locked}");
    assert!(structured(&locked)["flow_lock"].is_object());
    assert!(structured(&locked)["flow_lock"]["flow"].is_object());
    assert!(structured(&locked)["flow_lock"].get("bytes").is_none());
    assert!(structured(&locked)["flow_lock"].get("json").is_none());
    let lock_id = structured(&locked)["flow_lock_id"].as_str().unwrap();
    let content_hash = structured(&locked)["content_hash"].as_str().unwrap();

    drop(first);
    let mut second = server(root.join("second-assets"));
    let loaded = call_tool(
        &mut second,
        2,
        "flow_apply",
        json!({ "package_path": package }),
    );

    assert_eq!(structured(&loaded)["ok"], true, "{loaded}");
    assert_eq!(structured(&loaded)["flow_lock_id"], lock_id);
    assert_eq!(structured(&loaded)["content_hash"], content_hash);

    let prepared = call_tool(
        &mut second,
        3,
        "prepare_flow_review",
        json!({ "package_path": package }),
    );
    assert_eq!(structured(&prepared)["ok"], true, "{prepared}");
    assert_eq!(structured(&prepared)["flow_lock_id"], lock_id);
    fs::remove_dir_all(root).unwrap();
}
