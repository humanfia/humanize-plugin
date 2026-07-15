use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::adapters::tmux::{CommandOutput, CommandRunner, TmuxError};
use crate::driver::protocol::{CursorPolicy, DriverWire};
use crate::driver::{DriverIpcMetadata, socket_path_for_run_root};
use crate::flow::{self, FlowCheckMode, FlowSuggestInput};
use crate::review::ReviewDecision;
use crate::run_assets::{RunAssetSink, RunAssetStore};

use super::driver_proxy::ToolArgumentContext;
use super::flow_json::flow_draft_json;
use super::participant::{McpCaller, ParticipantCaller};
use super::registry::{
    AuthoringOperation, CallerKind, TOOL_SPECS, ToolCategory, ToolRoute, advertised_specs,
    advertised_specs_for,
};
use super::{McpServer, McpSurface};

const RUN_ID: &str = "registry-contract-run";
const TOKEN: &str = "registry-contract-token";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);
static STATE_ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn tool_specs_are_the_exact_descriptor_surface() {
    let mut server = McpServer::with_tmux_runner(NoopRunner);
    let surface = McpSurface;
    let listed = server
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }))
        .expect("tools/list should respond");
    let listed_tools = listed["result"]["tools"]
        .as_array()
        .expect("tools/list should return descriptors");
    let listed_names = listed_tools
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("listed tool name should be a string")
        })
        .collect::<BTreeSet<_>>();
    let advertised_names = advertised_specs()
        .map(|spec| spec.name)
        .collect::<BTreeSet<_>>();
    assert_eq!(listed_names, advertised_names);
    assert_eq!(listed_tools.len(), advertised_names.len());
    assert_eq!(advertised_specs().count(), advertised_names.len());

    let mut all_names = BTreeSet::new();
    for spec in TOOL_SPECS {
        assert!(all_names.insert(spec.name), "duplicate spec {}", spec.name);
        let schema = (spec.input_schema)();
        assert_eq!(schema["type"], "object", "schema for {}", spec.name);
        assert!(!spec.description.trim().is_empty(), "{}", spec.name);

        let descriptor = surface.lookup(spec.name);
        let listed_descriptor = listed_tools.iter().find(|tool| tool["name"] == spec.name);
        if spec.is_advertised_for(CallerKind::Operator) {
            let descriptor = descriptor.expect("advertised spec should have a descriptor");
            assert_eq!(descriptor.name(), spec.name);
            assert_eq!(descriptor.description(), spec.description);
            assert_eq!(descriptor.input_schema(), &schema);
            let listed_descriptor =
                listed_descriptor.expect("advertised spec should appear in tools/list");
            assert_eq!(listed_descriptor["description"], spec.description);
            assert_eq!(listed_descriptor["inputSchema"], schema);

            let category_descriptors = match spec.category {
                ToolCategory::Runtime => surface.runtime_tools(),
                ToolCategory::Authoring => surface.authoring_tools(),
                ToolCategory::Review => surface.review_tools(),
            };
            assert!(
                category_descriptors
                    .iter()
                    .any(|descriptor| descriptor.name() == spec.name),
                "{} missing from its category surface",
                spec.name
            );
        } else {
            assert!(descriptor.is_none(), "hidden descriptor {}", spec.name);
            assert!(
                listed_descriptor.is_none(),
                "hidden tools/list entry {}",
                spec.name
            );
            let response = call_tool(&mut server, spec.name, json!({}));
            assert_eq!(response["error"]["message"], "unknown tool");
        }
    }
}

#[test]
fn tool_specs_exhaustively_bind_routes_wires_and_cursor_policy() {
    let mut registered_wires = BTreeSet::new();
    for spec in TOOL_SPECS {
        assert_eq!(
            spec.is_advertised_for(CallerKind::Operator),
            spec.is_advertised_for(CallerKind::Operator)
        );
        let expected_cursor = match spec.route {
            ToolRoute::DriverMutation {
                bootstrap: false, ..
            }
            | ToolRoute::ParticipantMessage(_) => CursorPolicy::ExpectedAuthority,
            ToolRoute::Authoring(_)
            | ToolRoute::DriverRead(_)
            | ToolRoute::DriverMutation {
                bootstrap: true, ..
            }
            | ToolRoute::Hidden => CursorPolicy::None,
        };
        let actual_cursor = spec
            .route
            .wire()
            .map(DriverWire::cursor_policy)
            .unwrap_or(CursorPolicy::None);
        assert_eq!(actual_cursor, expected_cursor, "{}", spec.name);
        if let Some(wire) = spec.route.wire() {
            registered_wires.insert(wire);
        }
        if spec.category == ToolCategory::Runtime {
            assert!(!matches!(spec.route, ToolRoute::Authoring(_)));
        }
    }
    assert_eq!(
        registered_wires,
        DriverWire::ALL.iter().copied().collect::<BTreeSet<_>>()
    );
}

#[test]
fn every_tool_spec_drives_real_call_and_proxy_behavior() {
    let fixture = FakeDriverFixture::new();
    let mut server = fixture.server();
    let (lock_id, lock_hash, review_id, flow_value) = install_flow_lock(&mut server);

    for spec in TOOL_SPECS {
        let arguments = arguments_for(
            spec.route,
            spec.input_schema,
            &lock_id,
            &lock_hash,
            &review_id,
            &flow_value,
        );
        if !spec.is_advertised_for(CallerKind::Operator) {
            let response = call_tool(&mut server, spec.name, arguments);
            assert_eq!(
                response["error"]["message"], "unknown tool",
                "{}",
                spec.name
            );
            continue;
        }
        let expected_arguments = {
            let context = ToolArgumentContext::new(&server.state, &server.caller);
            (spec.prepare_arguments)(&context, &arguments)
                .unwrap_or_else(|error| panic!("{} preparer failed: {}", spec.name, error.message))
        };
        let response = call_tool(&mut server, spec.name, arguments);

        match spec.route {
            ToolRoute::Authoring(_) => {
                assert_not_unknown(spec.name, &response);
            }
            ToolRoute::DriverMutation {
                bootstrap: true, ..
            } => {
                assert_not_unknown(spec.name, &response);
                assert_eq!(
                    response["result"]["structuredContent"]["attached"], true,
                    "{}",
                    spec.name
                );
                assert_eq!(expected_arguments["run_id"], RUN_ID);
            }
            ToolRoute::DriverRead(wire)
            | ToolRoute::DriverMutation {
                wire,
                bootstrap: false,
            }
            | ToolRoute::ParticipantMessage(wire) => {
                let structured = &response["result"]["structuredContent"];
                assert_eq!(structured["observed_op"], wire.as_str(), "{}", spec.name);
                assert_eq!(
                    structured["observed_arguments"], expected_arguments,
                    "{}",
                    spec.name
                );
            }
            ToolRoute::Hidden => unreachable!("hidden tools are rejected above"),
        }
    }
}

#[test]
fn participant_tool_specs_drive_descriptor_schema_preparation_and_proxy_behavior() {
    let fixture = FakeDriverFixture::new();
    let mut server = fixture.server();
    server.caller = McpCaller::Participant(ParticipantCaller {
        run_id: RUN_ID.to_string(),
        activation_id: "root".to_string(),
        handle: "participant-handle".to_string(),
        credential: "participant-credential".to_string(),
        runs_root: fixture.root.join("runs"),
    });
    let listed = server
        .handle_json_rpc(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .unwrap();
    let listed_tools = listed["result"]["tools"].as_array().unwrap();
    let listed_names = listed_tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let expected_names = advertised_specs_for(CallerKind::Participant)
        .map(|spec| spec.name)
        .collect::<BTreeSet<_>>();
    assert_eq!(listed_names, expected_names);

    let surface = McpSurface;
    for spec in TOOL_SPECS {
        let descriptor = surface.lookup_for(spec.name, CallerKind::Participant);
        if !spec.is_advertised_for(CallerKind::Participant) {
            assert!(descriptor.is_none(), "{}", spec.name);
            let response = call_tool(&mut server, spec.name, json!({}));
            assert_eq!(
                response["error"]["message"], "unknown tool",
                "{}",
                spec.name
            );
            continue;
        }
        let descriptor = descriptor.expect("participant descriptor should exist");
        assert_eq!(
            descriptor.input_schema(),
            &spec.input_schema_for(CallerKind::Participant)
        );
        let arguments = participant_arguments(spec.route);
        let expected_arguments = {
            let context = ToolArgumentContext::new(&server.state, &server.caller);
            (spec.prepare_arguments)(&context, &arguments).unwrap()
        };
        let response = call_tool(&mut server, spec.name, arguments);
        let wire = spec.route.wire().expect("participant tool must proxy");
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["observed_op"], wire.as_str(), "{}", spec.name);
        let mut public_arguments = expected_arguments.clone();
        let public_object = public_arguments
            .as_object_mut()
            .expect("prepared participant arguments should be an object");
        public_object.remove("participant_handle");
        public_object.remove("participant_credential");
        assert_eq!(
            structured["observed_arguments"], public_arguments,
            "{}",
            spec.name
        );
        assert_eq!(
            structured["observed_participant_authority"], true,
            "{}",
            spec.name
        );
        let public_bytes = serde_json::to_vec(structured).unwrap();
        assert!(
            !public_bytes
                .windows("participant-handle".len())
                .any(|window| { window == "participant-handle".as_bytes() })
        );
        assert!(
            !public_bytes
                .windows("participant-credential".len())
                .any(|window| { window == "participant-credential".as_bytes() })
        );
    }
}

fn participant_arguments(route: ToolRoute) -> Value {
    match route.wire().expect("participant route must have a wire") {
        DriverWire::Context | DriverWire::ValidateStop => json!({}),
        DriverWire::DeliverArtifact => {
            json!({"artifact_key":"artifact", "payload":{"value":1}})
        }
        DriverWire::RecordEffect => {
            json!({"effect_key":"effect", "payload":{"value":1}})
        }
        wire => panic!("unexpected participant wire {wire:?}"),
    }
}

fn arguments_for(
    route: ToolRoute,
    schema_builder: fn() -> Value,
    lock_id: &str,
    lock_hash: &str,
    review_id: &str,
    flow_value: &Value,
) -> Value {
    match route {
        ToolRoute::Hidden => json!({}),
        ToolRoute::Authoring(operation) => {
            authoring_arguments(operation, lock_id, lock_hash, flow_value)
        }
        ToolRoute::DriverMutation {
            wire: DriverWire::BindRun,
            bootstrap: true,
        } => json!({
            "runId": RUN_ID,
            "flowLockId": lock_id,
            "contentHash": lock_hash,
            "reviewId": review_id
        }),
        ToolRoute::DriverRead(wire)
        | ToolRoute::DriverMutation { wire, .. }
        | ToolRoute::ParticipantMessage(wire) => {
            driver_arguments(wire, schema_builder(), lock_id, lock_hash)
        }
    }
}

fn authoring_arguments(
    operation: AuthoringOperation,
    lock_id: &str,
    lock_hash: &str,
    flow_value: &Value,
) -> Value {
    match operation {
        AuthoringOperation::FlowRepair
        | AuthoringOperation::FlowCheck
        | AuthoringOperation::FlowLock => json!({ "flow": flow_value }),
        AuthoringOperation::FlowApply => json!({ "flow_lock_id": lock_id }),
        AuthoringOperation::FlowSuggest => json!({
            "goal": "Registry contract",
            "readme": "Registry contract workflow."
        }),
        AuthoringOperation::FlowExport => {
            json!({ "flow_lock_id": lock_id, "format": "json" })
        }
        AuthoringOperation::ProposeFlowUpdate => json!({ "flow": flow_value }),
        AuthoringOperation::PrepareFlowReview => json!({
            "flow_lock_id": lock_id,
            "content_hash": lock_hash
        }),
        AuthoringOperation::DecideFlowReview => json!({
            "review_id": "missing-review",
            "decision": "approved"
        }),
    }
}

fn driver_arguments(wire: DriverWire, schema: Value, lock_id: &str, lock_hash: &str) -> Value {
    let mut arguments = Map::new();
    arguments.insert("runId".into(), json!(RUN_ID));
    if wire.cursor_policy() == CursorPolicy::ExpectedAuthority {
        arguments.insert("expectedEventCursor".into(), json!(0));
        arguments.insert("expectedContextGeneration".into(), json!(0));
    }
    match wire {
        DriverWire::BindRun
        | DriverWire::Context
        | DriverWire::Status
        | DriverWire::Why
        | DriverWire::Pause
        | DriverWire::Resume
        | DriverWire::Complete
        | DriverWire::Stop
        | DriverWire::ViewTerminal
        | DriverWire::ViewSnapshot => {
            if wire == DriverWire::BindRun {
                arguments.insert("reviewId".into(), json!("review-test"));
            }
        }
        DriverWire::DeliverArtifact => {
            arguments.insert("activationId".into(), json!("root"));
            arguments.insert("artifactKey".into(), json!("artifact"));
            arguments.insert("payload".into(), json!({ "value": 1 }));
        }
        DriverWire::PatchBoard => {
            arguments.insert("activationId".into(), json!("root"));
            arguments.insert("expectedVersion".into(), json!(0));
            arguments.insert("patch".into(), json!({ "value": 1 }));
        }
        DriverWire::RecordEffect => {
            arguments.insert("activationId".into(), json!("root"));
            arguments.insert("effectKey".into(), json!("effect"));
            arguments.insert("payload".into(), json!({ "value": 1 }));
        }
        DriverWire::ValidateStop => {
            arguments.insert("activationId".into(), json!("root"));
        }
        DriverWire::ObserveStop => {
            arguments.insert("activationId".into(), json!("root"));
            arguments.insert("reason".into(), json!("complete"));
        }
        DriverWire::Activate => {
            arguments.insert("nodeId".into(), json!("child"));
            arguments.insert("requiredArtifacts".into(), json!(["child-output"]));
        }
        DriverWire::Fanout => {
            arguments.insert("nodeId".into(), json!("child"));
            arguments.insert("artifactKey".into(), json!("artifact"));
        }
        DriverWire::ApplyFlowRevision => {
            arguments.insert("flowLockId".into(), json!(lock_id));
            arguments.insert("contentHash".into(), json!(lock_hash));
            arguments.insert("reviewId".into(), json!("review-test"));
            if schema["properties"].get("mode").is_some() {
                arguments.insert("mode".into(), json!("future_activations"));
            } else {
                arguments.insert("applyMode".into(), json!("future_activations"));
            }
        }
        DriverWire::PreviewFlowRoutes => {}
        DriverWire::RecordHookFact => {
            arguments.insert("sessionId".into(), json!("host-session"));
            arguments.insert("activationId".into(), json!("root"));
            arguments.insert("hook".into(), json!("compaction_pending"));
        }
        DriverWire::SendMessage => {
            arguments.insert("activationId".into(), json!("root"));
            arguments.insert("messageId".into(), json!("message-1"));
            arguments.insert("message".into(), json!("hello"));
        }
    }
    Value::Object(arguments)
}

fn install_flow_lock(server: &mut McpServer<NoopRunner>) -> (String, String, String, Value) {
    let draft = flow::flow_suggest(FlowSuggestInput {
        goal: "Registry contract".into(),
        readme: "Registry contract workflow.".into(),
        nodes: vec!["root".into()],
        artifact: Some("artifact".into()),
    })
    .expect("flow suggestion should succeed");
    let flow_value = flow_draft_json(&draft);
    let lock = flow::flow_lock(&draft, FlowCheckMode::Core).expect("flow should lock");
    let lock_id = lock.id().to_string();
    let lock_hash = lock.content_hash().to_string();
    let review = server
        .review_store
        .prepare(
            &lock,
            &json!({"title": "Registry contract review"}),
            "<title>Registry contract review</title>\n",
        )
        .expect("review should prepare");
    let review = server
        .review_store
        .decide(review.review_id(), ReviewDecision::Approved, None)
        .expect("review should approve");
    let review_id = review.review_id().to_string();
    server.state.flow_locks.insert(lock_id.clone(), lock);
    (lock_id, lock_hash, review_id, flow_value)
}

fn call_tool(server: &mut McpServer<NoopRunner>, name: &str, arguments: Value) -> Value {
    server
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": name,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }))
        .expect("tools/call should respond")
}

fn assert_not_unknown(name: &str, response: &Value) {
    assert_ne!(
        response["error"]["message"], "unknown tool",
        "{name} was not dispatched"
    );
}

#[derive(Clone, Copy)]
struct NoopRunner;

impl CommandRunner for NoopRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        panic!("registry contract unexpectedly invoked tmux: {argv:?}")
    }
}

struct FakeDriverFixture {
    root: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    socket_path: PathBuf,
    prior_state_root: Option<OsString>,
    _state_env_guard: MutexGuard<'static, ()>,
}

impl FakeDriverFixture {
    fn new() -> Self {
        let state_env_guard = STATE_ENV_LOCK
            .lock()
            .expect("state environment lock should be available");
        let root = std::env::temp_dir().join(format!(
            "humanize-plugin-registry-contract-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::SeqCst)
        ));
        if root.exists() {
            fs::remove_dir_all(&root).expect("old registry fixture should be removable");
        }
        let prior_state_root = std::env::var_os("HUMANIZE_STATE_ROOT");
        unsafe {
            std::env::set_var("HUMANIZE_STATE_ROOT", &root);
        }
        let runs_root = root.join("runs");
        let runtime_root = root.join("runtime");
        fs::create_dir_all(&runtime_root).expect("runtime root should be created");
        set_mode(&runtime_root, 0o700);
        let store = RunAssetStore::new(RunAssetSink::HumanizeRunsDir(runs_root));
        store
            .start_run_manifest(RUN_ID)
            .expect("run manifest should be created");
        let run_root = store.run_root(RUN_ID).expect("run root should resolve");
        crate::private_state::ensure_run_identity(
            &runtime_root,
            &run_root,
            &root.join("runs"),
            RUN_ID,
        )
        .expect("private run identity should be created");
        let private_run_root = private_run_root_for_run_root(&runtime_root, &run_root);
        fs::create_dir_all(&private_run_root).expect("private run root should be created");
        set_mode(&private_run_root, 0o700);
        let driver_dir = private_run_root.join("driver");
        fs::create_dir_all(&driver_dir).expect("driver directory should be created");
        set_mode(&driver_dir, 0o700);

        let socket_path = socket_path_for_run_root(&runtime_root, &run_root);
        let listener = UnixListener::bind(&socket_path).expect("fake driver socket should bind");
        set_mode(&socket_path, 0o600);
        listener
            .set_nonblocking(true)
            .expect("fake listener should be nonblocking");
        write_private(
            &driver_dir.join("ipc-token"),
            format!("{TOKEN}\n").as_bytes(),
        );
        let metadata = DriverIpcMetadata {
            run_id: RUN_ID.into(),
            socket_path: PathBuf::from(
                socket_path
                    .file_name()
                    .expect("socket should have a file name"),
            ),
            auth_token_path: PathBuf::from("ipc-token"),
            updated_at_ms: 1,
        };
        write_private(
            &driver_dir.join("ipc.json"),
            &serde_json::to_vec(&metadata).expect("metadata should serialize"),
        );

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || serve_fake_driver(listener, thread_stop));
        Self {
            root,
            stop,
            thread: Some(thread),
            socket_path,
            prior_state_root,
            _state_env_guard: state_env_guard,
        }
    }

    fn server(&self) -> McpServer<NoopRunner> {
        McpServer::with_tmux_runner_and_run_asset_store(
            NoopRunner,
            RunAssetStore::new(RunAssetSink::HumanizeRunsDir(self.root.join("runs"))),
        )
    }
}

impl Drop for FakeDriverFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket_path);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("fake driver thread should stop");
        }
        let _ = fs::remove_dir_all(&self.root);
        unsafe {
            match self.prior_state_root.take() {
                Some(value) => std::env::set_var("HUMANIZE_STATE_ROOT", value),
                None => std::env::remove_var("HUMANIZE_STATE_ROOT"),
            }
        }
    }
}

fn serve_fake_driver(listener: UnixListener, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => respond_to_fake_driver_request(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("fake driver accept failed: {error}"),
        }
    }
}

fn private_run_root_for_run_root(runtime_root: &Path, run_root: &Path) -> PathBuf {
    let identity = std::path::absolute(run_root)
        .unwrap_or_else(|_| run_root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    runtime_root.join(format!("r{:016x}", stable_hash(&identity)))
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn respond_to_fake_driver_request(mut stream: UnixStream) {
    let mut line = String::new();
    BufReader::new(stream.try_clone().expect("stream should clone"))
        .read_line(&mut line)
        .expect("fake driver request should be readable");
    let Ok(mut request) = serde_json::from_str::<Value>(&line) else {
        return;
    };
    let op = request["op"].as_str().unwrap_or_default().to_string();
    let observed_participant_authority = request.get("participant_handle").is_some()
        && request.get("participant_credential").is_some();
    let object = request
        .as_object_mut()
        .expect("driver request should be an object");
    object.remove("id");
    object.remove("token");
    object.remove("op");
    let response = if op == "status" {
        json!({
            "ok": true,
            "run_id": RUN_ID,
            "run_status": "running",
            "run_mode": "finite",
            "initial_activation_limit": u64::MAX,
            "activation_limit": u64::MAX,
            "activations_used": 0,
            "event_cursor": 0,
            "context_generation": 0,
            "observed_op": op,
            "observed_arguments": request,
            "observed_participant_authority": observed_participant_authority,
            "context": {
                "run_id": RUN_ID,
                "run_status": "running",
                "activations": {},
                "flow_revisions": []
            }
        })
    } else {
        json!({
            "ok": true,
            "run_id": RUN_ID,
            "observed_op": op,
            "observed_arguments": request,
            "observed_participant_authority": observed_participant_authority
        })
    };
    stream
        .write_all(
            format!(
                "{}\n",
                serde_json::to_string(&response).expect("response should serialize")
            )
            .as_bytes(),
        )
        .expect("fake driver response should be writable");
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap_or_else(|error| panic!("create {} failed: {error}", path.display()));
    file.write_all(bytes)
        .unwrap_or_else(|error| panic!("write {} failed: {error}", path.display()));
    file.sync_all()
        .unwrap_or_else(|error| panic!("sync {} failed: {error}", path.display()));
}

fn set_mode(path: &Path, mode: u32) {
    let mut permissions = fs::symlink_metadata(path)
        .unwrap_or_else(|error| panic!("stat {} failed: {error}", path.display()))
        .permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
        .unwrap_or_else(|error| panic!("chmod {} failed: {error}", path.display()));
}
