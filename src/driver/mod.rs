use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::adapters::tmux::{
    SystemCommandRunner, TmuxAdapter, TmuxInputTransactionConfig, TmuxPipeCapture,
};
use crate::flow;
use crate::input_ledger::MachineInputLedger;
use crate::review::ReviewStore;
use crate::run_assets::{RunAssetError, RunAssetSink, RunAssetStore};
use crate::runtime::{self, BoardPatch, ControlCommand, DriverTickInput, NodeSpec, StopContract};

mod actuation;
mod capabilities;
mod client;
mod delivery;
mod flow_lock;
mod ipc;
mod participant;
mod persistence;
mod process_signal;
pub(crate) mod protocol;
mod publication;
mod publication_obligations;
mod read_model;
mod recovery;
mod run_lifecycle;
mod storage;
mod tmux_ownership;
mod wire_format;

pub use client::{DriverClient, DriverIpcMetadata, cleanup_stale_driver_ipc};
pub(crate) use client::{
    DriverEndpointState, private_driver_dir, probe_driver_endpoint, runtime_root_for_run_root,
};
pub use recovery::{
    DriverAttachLock, DriverRecoveryState, DriverRecoveryTmux, acquire_driver_attach_lock,
    load_driver_recovery_state,
};

use client::{private_run_root_for_run_root, write_ipc_metadata};
use delivery::{AmbiguousDelivery, SubmittedDelivery, input_delivery_resolution_from_request};
use flow_lock::StoredFlowRevision;
use ipc::{FrameRead, IO_TIMEOUT, connect_with_timeout, read_frame, write_frame};
use participant::ParticipantBinding;
use persistence::{read_driver_events, read_runtime_referenced_locks, replay_driver_events};
use process_signal::ProcessSignalGuard;
use protocol::{CursorPolicy, DriverWire, wire_from_name};
use run_lifecycle::{TmuxPaneCleanupIntent, TmuxPipeCaptureIntent};
use storage::{
    append_json_line_private, remove_regular_file, remove_stale_socket, unix_time_ms,
    validate_run_id,
};
pub(crate) use tmux_ownership::parse_tmux_actuation_config;
use tmux_ownership::{DriverTmuxState, StoredPane, TmuxPaneAllocationIntent};
use wire_format::{
    activation_status_name, driver_error, effects_json, flow_lock_mode_name, optional_u64_field,
    parse_context_payloads, parse_payload_value, payload_string, required_string,
    route_decisions_json, run_mode_name, run_status_name, stop_decision_kind_name,
    stop_decisions_json, stop_validation_error_json, string_field, with_id,
};

const EVENTS_FILE: &str = "events.jsonl";
const IPC_FILE: &str = "ipc.json";
const SNAPSHOT_FILE: &str = "snapshot.json";
const DRIVER_EVENTS_FILE: &str = "driver-events.jsonl";
const REVISIONS_DIR: &str = "revisions";
const RUNTIME_EVENT_BATCH_PROTOCOL: &str = "humanize.driver.runtime_event_batch.v1";
const MAX_IPC_CONNECTIONS: usize = 32;

#[derive(Debug, Clone)]
pub struct DriverConfig {
    pub run_id: String,
    pub runs_root: PathBuf,
    pub runtime_root: PathBuf,
    pub auth_token: String,
    pub auth_token_path: Option<PathBuf>,
    pub review_root: PathBuf,
    pub operator_pane: Option<DriverPaneConfig>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DriverPaneConfig {
    pub session_id: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
}

impl DriverConfig {
    pub fn socket_path(&self) -> io::Result<PathBuf> {
        let run_root = self.run_root()?;
        Ok(socket_path_for_run_root(
            &runtime_root_for_run_root(&run_root)?,
            &run_root,
        ))
    }

    fn run_root(&self) -> io::Result<PathBuf> {
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(self.runs_root.clone()))
            .run_root(&self.run_id)
            .map_err(|err| io::Error::other(err.to_string()))
    }
}

pub fn socket_path_for_run_root(runtime_root: &Path, run_root: &Path) -> PathBuf {
    private_run_root_for_run_root(runtime_root, run_root).join("s")
}

pub fn run_driver(config: DriverConfig) -> io::Result<()> {
    validate_run_id(&config.run_id)?;
    let signals = ProcessSignalGuard::install()?;
    let run_root = config.run_root()?;
    let expected_runtime_root = runtime_root_for_run_root(&run_root)?;
    if std::path::absolute(&config.runtime_root)? != std::path::absolute(&expected_runtime_root)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "driver runtime root does not match the authoritative run directory",
        ));
    }
    let private_run_root =
        crate::private_state::ensure_private_run_root(&config.runtime_root, &run_root)?;
    crate::private_state::ensure_run_identity(
        &config.runtime_root,
        &run_root,
        &config.runs_root,
        &config.run_id,
    )?;
    let driver_dir = private_run_root.join("driver");
    crate::private_state::ensure_private_directory(&driver_dir)?;
    let socket_path = config.socket_path()?;
    remove_stale_socket(&socket_path, &config.runtime_root)?;
    let service = match RuntimeDriverService::load(config.clone()) {
        Ok(service) => Arc::new(Mutex::new(service)),
        Err(err) => return Err(err),
    };
    let listener = UnixListener::bind(&socket_path)?;
    let mut socket_permissions = fs::metadata(&socket_path)?.permissions();
    use std::os::unix::fs::PermissionsExt as _;
    socket_permissions.set_mode(0o600);
    fs::set_permissions(&socket_path, socket_permissions)?;
    listener.set_nonblocking(true)?;
    if let Err(err) = write_ipc_metadata(&config) {
        drop(listener);
        let _ = remove_stale_socket(&socket_path, &config.runtime_root);
        return Err(err);
    }
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let listener_shutdown = Arc::new(AtomicBool::new(false));
    let ipc_service = Arc::clone(&service);
    let ipc_shutdown_requested = Arc::clone(&shutdown_requested);
    let ipc_listener_shutdown = Arc::clone(&listener_shutdown);
    let ipc_token = config.auth_token.clone();
    let ipc_thread = thread::spawn(move || {
        serve_ipc(
            listener,
            ipc_service,
            ipc_listener_shutdown,
            ipc_shutdown_requested,
            ipc_token,
        );
    });

    println!("driver ready run_id={}", config.run_id);
    let console_service = Arc::clone(&service);
    let console_shutdown = Arc::clone(&shutdown_requested);
    let _console_thread = thread::spawn(move || {
        let stdin = io::stdin();
        let mut stdout = io::stdout();
        for line in stdin.lock().lines() {
            let Ok(line) = line else {
                break;
            };
            let detach = line.trim() == "detach";
            let output = handle_console_command(&line, &console_service, &console_shutdown);
            if let Some(output) = output {
                let _ = writeln!(stdout, "{output}");
                let _ = stdout.flush();
            }
            if detach || console_shutdown.load(Ordering::SeqCst) {
                break;
            }
        }
    });
    while !shutdown_requested.load(Ordering::SeqCst) && !signals.received() {
        thread::sleep(Duration::from_millis(50));
    }
    let exit_reason = if signals.received() {
        "driver_process_signal"
    } else {
        "driver_shutdown"
    };
    let _attach_lock = acquire_driver_attach_lock(&run_root)?;
    listener_shutdown.store(true, Ordering::SeqCst);
    let _ = connect_with_timeout(&socket_path, IO_TIMEOUT);
    let _ = ipc_thread.join();
    let finalization = service
        .lock()
        .map_err(|_| io::Error::other("driver state is unavailable"))?
        .finalize_for_exit(exit_reason)
        .map_err(|err| io::Error::other(err.message));
    let _ = remove_stale_socket(&socket_path, &config.runtime_root);
    let _ = remove_regular_file(&driver_dir.join(IPC_FILE));
    let _ = remove_regular_file(&driver_dir.join("ipc-token"));
    finalization.map(|_| ())
}

fn serve_ipc(
    listener: UnixListener,
    service: Arc<Mutex<RuntimeDriverService>>,
    listener_shutdown: Arc<AtomicBool>,
    shutdown_requested: Arc<AtomicBool>,
    auth_token: String,
) {
    let active = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    while !listener_shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                workers.retain(|worker: &thread::JoinHandle<()>| !worker.is_finished());
                if !reserve_connection(&active) {
                    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
                    let _ = write_frame(
                        &mut stream,
                        &driver_error(
                            Value::Null,
                            "driver_busy",
                            "driver IPC connection limit reached",
                        ),
                    );
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                let worker_service = Arc::clone(&service);
                let worker_shutdown = Arc::clone(&shutdown_requested);
                let worker_token = auth_token.clone();
                let worker_active = Arc::clone(&active);
                workers.push(thread::spawn(move || {
                    handle_connection(stream, &worker_service, &worker_shutdown, &worker_token);
                    worker_active.fetch_sub(1, Ordering::SeqCst);
                }));
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
}

fn reserve_connection(active: &AtomicUsize) -> bool {
    let mut current = active.load(Ordering::SeqCst);
    loop {
        if current >= MAX_IPC_CONNECTIONS {
            return false;
        }
        match active.compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(updated) => current = updated,
        }
    }
}

fn handle_connection(
    mut stream: UnixStream,
    service: &Arc<Mutex<RuntimeDriverService>>,
    shutdown: &Arc<AtomicBool>,
    auth_token: &str,
) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let (response, participant_exit_binding) = match read_frame(&mut stream) {
        Ok(FrameRead::Complete(frame)) => match std::str::from_utf8(&frame) {
            Ok(line) => handle_ipc_line(line, service, shutdown, auth_token),
            Err(_) => (
                driver_error(
                    Value::Null,
                    "malformed_request",
                    "request must be valid UTF-8 JSON",
                ),
                None,
            ),
        },
        Ok(FrameRead::TooLarge) => (
            driver_error(
                Value::Null,
                "request_too_large",
                "driver IPC request exceeds the maximum frame size",
            ),
            None,
        ),
        Ok(FrameRead::Truncated) => (
            driver_error(
                Value::Null,
                "truncated_request",
                "driver IPC request must end with a newline",
            ),
            None,
        ),
        Ok(FrameRead::TimedOut) => (
            driver_error(
                Value::Null,
                "request_timeout",
                "driver IPC request read deadline exceeded",
            ),
            None,
        ),
        Err(_) => (
            driver_error(
                Value::Null,
                "malformed_request",
                "request could not be read",
            ),
            None,
        ),
    };
    let response_written = write_frame(&mut stream, &response).is_ok();
    let requires_ack = response
        .get("response_ack_required")
        .and_then(Value::as_bool)
        == Some(true);
    let response_acked = response_written && requires_ack && response_acknowledged(&mut stream);
    let participant_exit_cleanup = response_written
        && response.get("ok").and_then(Value::as_bool) == Some(true)
        && response.get("participant_exited").and_then(Value::as_bool) == Some(true)
        && (response_acked || response.get("idempotent").and_then(Value::as_bool) == Some(true));
    if participant_exit_cleanup
        && let Some(binding) = participant_exit_binding
        && let Ok(mut service) = service.lock()
    {
        let _ = service.reconcile_exited_participant(&binding);
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn response_acknowledged(stream: &mut UnixStream) -> bool {
    let Ok(FrameRead::Complete(frame)) = read_frame(stream) else {
        return false;
    };
    serde_json::from_slice::<Value>(&frame)
        .ok()
        .and_then(|value| value.get("ack").and_then(Value::as_str).map(str::to_string))
        .is_some_and(|ack| ack == "response_received")
}

fn handle_ipc_line(
    line: &str,
    service: &Arc<Mutex<RuntimeDriverService>>,
    shutdown: &Arc<AtomicBool>,
    auth_token: &str,
) -> (Value, Option<ParticipantBinding>) {
    let request = match serde_json::from_str::<Value>(line) {
        Ok(Value::Object(object)) => object,
        Ok(_) | Err(_) => {
            return (
                driver_error(
                    Value::Null,
                    "malformed_request",
                    "request must be a JSON object",
                ),
                None,
            );
        }
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(token) = request.get("token").and_then(Value::as_str) else {
        return (
            driver_error(id, "unauthorized", "driver IPC token is required"),
            None,
        );
    };
    if token != auth_token {
        return (
            driver_error(id, "unauthorized", "driver IPC token is invalid"),
            None,
        );
    }
    let Some(op) = request
        .get("op")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return (
            driver_error(id, "malformed_request", "op is required"),
            None,
        );
    };
    let Some(run_id) = request
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return (
            driver_error(id, "malformed_request", "run_id is required"),
            None,
        );
    };

    let mut service = match service.lock() {
        Ok(service) => service,
        Err(_) => {
            return (
                driver_error(id, "driver_unavailable", "driver state is unavailable"),
                None,
            );
        }
    };
    let shutdown_requested = op == "shutdown" && run_id == service.config.run_id;
    let request = Value::Object(request);
    let response = service.handle_request(id.clone(), &op, &run_id, &request);
    let participant_exit_binding = if op == "participant_exited"
        && response.get("ok").and_then(Value::as_bool) == Some(true)
        && response.get("participant_exited").and_then(Value::as_bool) == Some(true)
    {
        service.exited_participant_binding(&request)
    } else {
        None
    };
    if shutdown_requested {
        shutdown.store(true, Ordering::SeqCst);
    }
    (response, participant_exit_binding)
}

fn handle_console_command(
    line: &str,
    service: &Arc<Mutex<RuntimeDriverService>>,
    shutdown: &Arc<AtomicBool>,
) -> Option<String> {
    let command = line.trim();
    if command.is_empty() {
        return None;
    }
    match command {
        "help" => Some(
            "commands: help status why pause resume complete stop activations revisions detach quit shutdown"
                .into(),
        ),
        "detach" => Some("driver detached".into()),
        "quit" | "shutdown" => {
            shutdown.store(true, Ordering::SeqCst);
            Some("driver stopping".into())
        }
        "status" => service.lock().ok().map(|service| service.console_status()),
        "why" => service.lock().ok().map(|service| service.console_why()),
        "pause" | "resume" | "complete" | "stop" => service
            .lock()
            .ok()
            .map(|mut service| service.console_control(command)),
        "activations" => service.lock().ok().map(|service| service.console_activations()),
        "revisions" => service.lock().ok().map(|service| service.console_revisions()),
        _ => Some("unknown command; try help".into()),
    }
}

struct RuntimeDriverService {
    config: DriverConfig,
    private_run_root: PathBuf,
    driver: runtime::DriverState,
    locks: BTreeMap<String, StoredFlowRevision>,
    tmux_adapter: TmuxAdapter<SystemCommandRunner>,
    tmux: Option<DriverTmuxState>,
    operator_pane: Option<StoredPane>,
    tmux_pipe_captures: BTreeMap<String, (u64, TmuxPipeCapture)>,
    run_asset_store: RunAssetStore,
    review_store: ReviewStore,
    agent_launch_submitted_activations: BTreeSet<(String, u64)>,
    settled_actuation_activations: BTreeSet<(String, u64)>,
    ambiguous_deliveries: BTreeMap<(String, String), AmbiguousDelivery>,
    submitted_deliveries: BTreeMap<(String, String), SubmittedDelivery>,
    allocation_generations: BTreeMap<String, u64>,
    pending_tmux_allocations: BTreeMap<String, TmuxPaneAllocationIntent>,
    pending_pipe_captures: BTreeMap<String, TmuxPipeCaptureIntent>,
    pending_tmux_cleanups: BTreeMap<String, TmuxPaneCleanupIntent>,
    participant_bindings: BTreeMap<(String, u64), ParticipantBinding>,
    published_record_sources: BTreeSet<String>,
    persisted_event_count: usize,
    driver_event_count: u64,
    publication_blocked: Option<String>,
}

impl RuntimeDriverService {
    fn load(config: DriverConfig) -> io::Result<Self> {
        let run_root = config.run_root()?;
        let private_run_root = private_run_root_for_run_root(&config.runtime_root, &run_root);
        let run_asset_store = RunAssetStore::new_driver_owned(
            RunAssetSink::HumanizeRunsDir(config.runs_root.clone()),
            config.runtime_root.clone(),
        );
        let manifest = run_asset_store
            .load_or_start_run_manifest(&config.run_id)
            .map_err(|err| io::Error::other(err.to_string()))?;
        let events = publication::recover_runtime_events(&private_run_root)?;
        let persisted_event_count = events.len();
        let mut runtime_locks = read_runtime_referenced_locks(&private_run_root, &events)?;
        let runtime = runtime::Runtime::from_events(events);
        let driver_events = read_driver_events(&private_run_root)?;
        let driver_event_count = driver_events.len() as u64;
        let mut replay = replay_driver_events(&private_run_root, &driver_events)?;
        participant::project_participant_exits(runtime.events(), &mut replay.participant_bindings)?;
        replay.locks.append(&mut runtime_locks);
        let review_store = ReviewStore::new(config.review_root.clone());
        for revision in replay.locks.values().chain(runtime_locks.values()) {
            revision
                .authorize(&review_store)
                .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.message))?;
        }
        let tmux_pipe_captures = std::mem::take(&mut replay.pipe_captures)
            .into_iter()
            .map(|(activation_id, (allocation_generation, descriptor))| {
                (
                    activation_id,
                    (
                        allocation_generation,
                        TmuxPipeCapture::from_descriptor(
                            run_asset_store.activation_capture_root(&manifest),
                            descriptor,
                        ),
                    ),
                )
            })
            .collect();
        let tmux_adapter = TmuxAdapter::default().with_input_transaction_config(
            TmuxInputTransactionConfig::runtime_with_ledger(MachineInputLedger::at_path(
                private_run_root.join("driver").join("machine-inputs.jsonl"),
            )),
        );
        let mut service = Self {
            config,
            private_run_root,
            driver: runtime::DriverState::from_runtime(runtime),
            locks: replay.locks,
            tmux_adapter,
            tmux: replay.tmux,
            operator_pane: replay.operator_pane,
            tmux_pipe_captures,
            run_asset_store,
            review_store,
            agent_launch_submitted_activations: replay.agent_launch_submitted_activations,
            settled_actuation_activations: replay.settled_actuation_activations,
            ambiguous_deliveries: replay.ambiguous_deliveries,
            submitted_deliveries: replay.submitted_deliveries,
            allocation_generations: replay.allocation_generations,
            pending_tmux_allocations: replay.pending_tmux_allocations,
            pending_pipe_captures: replay.pending_pipe_captures,
            pending_tmux_cleanups: replay.pending_tmux_cleanups,
            participant_bindings: replay.participant_bindings,
            published_record_sources: BTreeSet::new(),
            persisted_event_count,
            driver_event_count,
            publication_blocked: None,
        };
        service
            .reconcile_publication_outbox()
            .map_err(|err| io::Error::other(err.message))?;
        service
            .refresh_published_record_sources()
            .map_err(|err| io::Error::other(err.message))?;
        let manifest = service
            .load_run_asset_manifest()
            .map_err(|err| io::Error::other(err.message))?;
        service
            .run_asset_store
            .reconcile_public_seal(&manifest, service.private_runtime_is_terminal(), false)
            .map_err(|err| io::Error::other(err.to_string()))?;
        service
            .reconcile_publication_obligations()
            .map_err(|err| io::Error::other(err.message))?;
        service
            .repair_publication_projections()
            .map_err(|err| io::Error::other(err.message))?;
        service
            .reconcile_stale_operator_pane()
            .map_err(|err| io::Error::other(err.message))?;
        service
            .reconcile_pending_tmux_cleanups()
            .map_err(|err| io::Error::other(err.message))?;
        service
            .reconcile_replayed_tmux_panes()
            .map_err(|err| io::Error::other(err.message))?;
        service
            .reconcile_exited_participants()
            .map_err(|err| io::Error::other(err.message))?;
        service
            .suspend_replayed_running_run()
            .map_err(|err| io::Error::other(err.message))?;
        service
            .register_configured_operator_pane()
            .map_err(|err| io::Error::other(err.message))?;
        Ok(service)
    }

    fn handle_request(&mut self, id: Value, op: &str, run_id: &str, request: &Value) -> Value {
        if run_id != self.config.run_id {
            return driver_error(id, "wrong_run", "driver owns a different run");
        }
        let wire = wire_from_name(op);
        let is_mutation = wire.is_some_and(DriverWire::is_mutation)
            || matches!(
                op,
                "participant_bind" | "participant_stop" | "participant_exited"
            );
        if is_mutation {
            if let Err(err) = self.reconcile_publication_outbox() {
                return DriverFailure::new(
                    "publication_blocked",
                    format!(
                        "pending public publication must reconcile before mutation: {}",
                        err.message
                    ),
                )
                .to_response(id);
            }
            let manifest = match self.load_run_asset_manifest() {
                Ok(manifest) => manifest,
                Err(err) => return err.to_response(id),
            };
            if let Err(err) = self.run_asset_store.reconcile_public_seal(
                &manifest,
                self.private_runtime_is_terminal(),
                true,
            ) {
                return DriverFailure::from_run_asset(err).to_response(id);
            }
            if let Err(err) = self.reconcile_publication_obligations() {
                return DriverFailure::new(
                    "publication_blocked",
                    format!(
                        "private publication obligations must reconcile before mutation: {}",
                        err.message
                    ),
                )
                .to_response(id);
            }
        }
        let result = match wire {
            Some(wire) => {
                if let Err(err) = self.authorize_participant_tool(wire, request) {
                    return err.to_response(id);
                }
                if wire.cursor_policy() == CursorPolicy::ExpectedAuthority
                    && let Err(err) = self.check_expected_authority(request)
                {
                    return err.to_response(id);
                }
                self.handle_tool_wire(wire, run_id, request)
            }
            None if op == "participant_bind" => self.bind_participant(request),
            None if op == "participant_stop" => self.participant_stop(request),
            None if op == "participant_exited" => self.participant_exited(request),
            None if op == "shutdown" => Ok(self.with_authority_fields(json!({
                "ok": true,
                "run_id": self.config.run_id,
                "shutdown": "requested"
            }))),
            None => Err(DriverFailure::new("unknown_op", "unknown driver operation")),
        };
        match result {
            Ok(response) => with_id(id, response),
            Err(err) => err.to_response(id),
        }
    }

    fn handle_tool_wire(
        &mut self,
        wire: DriverWire,
        run_id: &str,
        request: &Value,
    ) -> Result<Value, DriverFailure> {
        match wire {
            DriverWire::BindRun => self.bind_run(request),
            DriverWire::Context => self.get_context_response(request),
            DriverWire::Status => self.status_response(),
            DriverWire::Why => self.why_response(),
            DriverWire::Pause => self.control(
                ControlCommand::PauseRun {
                    run_id: run_id.to_string(),
                },
                request,
            ),
            DriverWire::Resume => self.control(
                ControlCommand::ResumeRun {
                    run_id: run_id.to_string(),
                },
                request,
            ),
            DriverWire::Complete => self.control(
                ControlCommand::CompleteRun {
                    run_id: run_id.to_string(),
                },
                request,
            ),
            DriverWire::Stop => self.control(
                ControlCommand::StopRun {
                    run_id: run_id.to_string(),
                },
                request,
            ),
            DriverWire::DeliverArtifact => self.deliver_artifact(request),
            DriverWire::PatchBoard => self.patch_board(request),
            DriverWire::RecordEffect => self.record_effect(request),
            DriverWire::ValidateStop => self.validate_stop(request),
            DriverWire::ObserveStop => self.observe_stop(request),
            DriverWire::Activate => self.activate_node(request),
            DriverWire::Fanout => self.fanout_from_artifact(request),
            DriverWire::ApplyFlowRevision => self.apply_flow_revision(request),
            DriverWire::PreviewFlowRoutes => self.preview_flow_routes(request),
            DriverWire::RecordHookFact => self.record_hook_fact(request),
            DriverWire::SendMessage => self.send_message(request),
            DriverWire::ViewTerminal => self.view_terminal_response(),
            DriverWire::ViewSnapshot => self.view_snapshot_response(),
        }
    }

    fn bind_run(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let run_mode = run_mode_from_request(request)?;
        let activation_limit = activation_limit_from_request(request)?;
        let stop_attempt_limit = stop_attempt_limit_from_request(request)?;

        if self.driver.runtime().has_run(&self.config.run_id) {
            let state = self.driver.runtime().state();
            let bound_lock_id = state
                .flow_lock_id_by_run
                .get(&self.config.run_id)
                .cloned()
                .ok_or_else(|| {
                    DriverFailure::new(
                        "run_binding_conflict",
                        "existing run has no immutable flow lock binding",
                    )
                })?;
            let bound_content_hash = state
                .contract_hash_by_run
                .get(&self.config.run_id)
                .cloned()
                .ok_or_else(|| {
                    DriverFailure::new(
                        "run_binding_conflict",
                        "existing run has no immutable content hash binding",
                    )
                })?;
            let package = if request.get("flow_lock").is_some() {
                StoredFlowRevision::from_request(request, &self.review_store)?
            } else {
                self.locks.get(&bound_lock_id).cloned().ok_or_else(|| {
                    DriverFailure::new(
                        "run_binding_conflict",
                        "existing run immutable flow lock package is unavailable",
                    )
                })?
            };
            let lock_id = package.lock_id().to_string();
            let content_hash = package.content_hash().to_string();
            let activation_ids = state
                .activations
                .values()
                .filter(|activation| activation.run_id == self.config.run_id)
                .map(|activation| activation.activation_id.clone())
                .collect::<Vec<_>>();
            let tmux_activation_ids = state
                .activations
                .values()
                .filter(|activation| activation.run_id == self.config.run_id)
                .filter(|activation| activation.status == runtime::ActivationStatus::Running)
                .map(|activation| activation.activation_id.clone())
                .collect::<Vec<_>>();
            let current_activation_limit = state.activation_limit(&self.config.run_id);
            let activations_used = state.activations_used(&self.config.run_id);
            if bound_lock_id != lock_id
                || bound_content_hash != content_hash
                || state.run_mode(&self.config.run_id) != Some(run_mode)
                || state.initial_activation_limit(&self.config.run_id) != Some(activation_limit)
                || state.stop_attempt_limit(&self.config.run_id) != Some(stop_attempt_limit)
            {
                return Err(DriverFailure::new(
                    "run_binding_conflict",
                    "existing run is bound to a different immutable flow lock or run configuration",
                ));
            }
            self.publish_applied_lock_revision(&lock_id, &content_hash, &package)?;
            self.publish_run_asset_flow_revision(&package)?;
            let tmux = self.bind_tmux_response(request, &tmux_activation_ids)?;
            return Ok(self.with_authority_fields(json!({
                "ok": true,
                "run_id": self.config.run_id,
                "run_status": run_status_name(self.run_status()),
                "flow_lock_id": lock_id,
                "content_hash": content_hash,
                "flow_lock": package.response_json(),
                "run_mode": run_mode_name(run_mode),
                "initial_activation_limit": activation_limit,
                "stop_attempt_limit": stop_attempt_limit,
                "activation_limit": current_activation_limit,
                "activations_used": activations_used,
                "activation_ids": activation_ids,
                "tmux": tmux,
                "pipeline": []
            })));
        }

        let package = StoredFlowRevision::from_request(request, &self.review_store)?;
        let lock = package.lock()?;
        let lock_id = package.lock_id().to_string();
        let content_hash = package.content_hash().to_string();
        self.write_lock_revision(&lock_id, &content_hash, &package)?;
        let initial_nodes = initial_node_specs(lock.draft());
        let activation_ids = initial_nodes
            .iter()
            .map(|node| node.id().to_string())
            .collect::<Vec<_>>();
        let mut next_driver = self.driver.clone();
        next_driver
            .runtime_mut()
            .start_run_with_limits(
                self.config.run_id.clone(),
                Vec::new(),
                run_mode,
                activation_limit,
                stop_attempt_limit,
            )
            .map_err(DriverFailure::from_runtime)?;
        next_driver
            .runtime_mut()
            .apply_flow_lock(
                self.config.run_id.clone(),
                runtime::FlowLockMode::FutureActivations,
                lock_id.clone(),
                content_hash.clone(),
            )
            .map_err(DriverFailure::from_runtime)?;
        next_driver
            .runtime_mut()
            .set_run_status(&self.config.run_id, runtime::RunStatus::Running)
            .map_err(DriverFailure::from_runtime)?;
        for node in &initial_nodes {
            next_driver
                .runtime_mut()
                .activate_node(&self.config.run_id, node, None)
                .map_err(DriverFailure::from_runtime)?;
        }
        let mut route_locks = self.route_locks();
        if !route_locks
            .iter()
            .any(|existing| existing.id() == lock.id())
        {
            route_locks.push(lock);
        }
        let report = next_driver.tick(route_tick_input(route_locks));
        self.commit_runtime_with_publication(next_driver, &report.route_decisions)?;
        self.publish_applied_lock_revision(&lock_id, &content_hash, &package)?;
        self.publish_run_asset_flow_revision(&package)?;
        let tmux = self.bind_tmux_response(request, &activation_ids)?;
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "run_status": run_status_name(self.run_status()),
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "flow_lock": package.response_json(),
            "run_mode": run_mode_name(run_mode),
            "initial_activation_limit": activation_limit,
            "stop_attempt_limit": stop_attempt_limit,
            "activation_limit": self.driver.runtime().state().activation_limit(&self.config.run_id),
            "activations_used": self.driver.runtime().state().activations_used(&self.config.run_id),
            "activation_ids": activation_ids,
            "tmux": tmux,
            "pipeline": report.pipeline
        })))
    }

    fn control(
        &mut self,
        command: ControlCommand,
        request: &Value,
    ) -> Result<Value, DriverFailure> {
        let should_allocate = matches!(&command, ControlCommand::ResumeRun { .. });
        let should_finalize = matches!(
            &command,
            ControlCommand::StopRun { .. } | ControlCommand::CompleteRun { .. }
        );
        let delivery_resolution = if should_allocate {
            input_delivery_resolution_from_request(request)?
        } else {
            None
        };
        if let Some(resolution) = delivery_resolution.as_ref() {
            self.validate_input_delivery_resolution(resolution)?;
        }
        if should_allocate
            && !matches!(
                self.run_status(),
                runtime::RunStatus::Paused | runtime::RunStatus::Quiescent
            )
        {
            return Err(DriverFailure::from_runtime(
                runtime::RuntimeError::InvalidRunStatusTransition {
                    run_id: self.config.run_id.clone(),
                    action: "resume".to_string(),
                    status: self.run_status(),
                },
            ));
        }
        let delivery_resolution = match delivery_resolution {
            Some(resolution) => Some(self.resolve_input_delivery(resolution)?),
            None => None,
        };
        if should_allocate && !self.ambiguous_deliveries.is_empty() {
            let warnings = self
                .ambiguous_deliveries
                .values()
                .map(AmbiguousDelivery::to_json)
                .collect();
            return Ok(self.with_authority_fields(json!({
                "ok": true,
                "run_id": self.config.run_id,
                "run_status": run_status_name(self.run_status()),
                "tmux_allocations": [],
                "actuation": actuation::DriverActuation::with_warnings(warnings).to_json(),
                "delivery_resolution": delivery_resolution,
                "resume_pending": true,
                "lifecycle": null,
                "pipeline": []
            })));
        }
        let mut next_driver = self.driver.clone();
        let mut input = route_tick_input(self.route_locks()).with_control(command);
        if should_allocate
            && let Some(activation_limit) =
                optional_u64_field(request, &["activation_limit", "activationLimit"])?
        {
            input = input.with_activation_limit(activation_limit);
        }
        let report = next_driver.tick(input);
        if let Some(error) = report.control_errors.first() {
            return Err(DriverFailure::from_runtime(error.clone()));
        }
        self.commit_runtime_with_publication(next_driver, &report.route_decisions)?;
        let (tmux_allocations, actuation) = self.reconcile_post_commit()?;
        let lifecycle = if should_finalize {
            Some(self.finalize_terminal_node_panes("run_terminal")?)
        } else {
            None
        };
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "run_status": run_status_name(self.run_status()),
            "tmux_allocations": tmux_allocations,
            "actuation": actuation.to_json(),
            "delivery_resolution": delivery_resolution,
            "lifecycle": lifecycle,
            "pipeline": report.pipeline
        })))
    }

    fn deliver_artifact(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let activation_id = required_string(request, "activation_id")?;
        let artifact_key = request
            .get("artifact_id")
            .or_else(|| request.get("artifact_key"))
            .or_else(|| request.get("artifactKey"))
            .or_else(|| request.get("key"))
            .and_then(Value::as_str)
            .ok_or_else(|| DriverFailure::new("malformed_request", "artifact id is required"))?;
        let payload = payload_string(request.get("payload"))?;
        let mut next_driver = self.driver.clone();
        let artifact_id = next_driver
            .runtime_mut()
            .deliver_artifact(&self.config.run_id, activation_id, artifact_key, payload)
            .map_err(DriverFailure::from_runtime)?;
        let report = next_driver.tick(route_tick_input(self.route_locks()));
        self.commit_runtime_with_publication(next_driver, &report.route_decisions)?;
        let (tmux_allocations, actuation) = self.reconcile_post_commit()?;
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "run_status": run_status_name(self.run_status()),
            "activation_id": activation_id,
            "artifact_key": artifact_key,
            "artifact_id": artifact_id,
            "pipeline": report.pipeline,
            "tmux_allocations": tmux_allocations,
            "actuation": actuation.to_json(),
            "route_decisions": route_decisions_json(&report.route_decisions)
        })))
    }

    fn patch_board(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let activation_id = required_string(request, "activation_id")?;
        let patch = request
            .get("patch")
            .and_then(Value::as_object)
            .ok_or_else(|| DriverFailure::new("malformed_request", "patch object is required"))?;
        let mut next_driver = self.driver.clone();
        let mut board_version = 0;
        let expected_version = optional_u64_field(request, &["expected_version"])?;
        for (key, value) in patch {
            let mut board_patch = BoardPatch::new(key, payload_string(Some(value))?)
                .map_err(|error| DriverFailure::new("malformed_request", error.to_string()))?;
            if let Some(expected_version) = expected_version {
                board_patch = board_patch.expect_version(expected_version);
            }
            board_version = next_driver
                .runtime_mut()
                .patch_board(&self.config.run_id, activation_id, board_patch)
                .map_err(DriverFailure::from_runtime)?;
        }
        let report = next_driver.tick(route_tick_input(self.route_locks()));
        self.commit_runtime_with_publication(next_driver, &report.route_decisions)?;
        let (tmux_allocations, actuation) = self.reconcile_post_commit()?;
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "run_status": run_status_name(self.run_status()),
            "activation_id": activation_id,
            "board_version": board_version,
            "tmux_allocations": tmux_allocations,
            "actuation": actuation.to_json(),
            "pipeline": report.pipeline
        })))
    }

    fn record_effect(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let activation_id = required_string(request, "activation_id")?;
        let effect_key = required_string(request, "effect_key")?;
        let payload = payload_string(request.get("payload"))?;
        let mut next_driver = self.driver.clone();
        next_driver
            .runtime_mut()
            .record_effect(&self.config.run_id, activation_id, effect_key, payload)
            .map_err(DriverFailure::from_runtime)?;
        let report = next_driver.tick(route_tick_input(self.route_locks()));
        self.commit_runtime_with_publication(next_driver, &report.route_decisions)?;
        let (tmux_allocations, actuation) = self.reconcile_post_commit()?;
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "run_status": run_status_name(self.run_status()),
            "activation_id": activation_id,
            "effect_key": effect_key,
            "tmux_allocations": tmux_allocations,
            "actuation": actuation.to_json(),
            "pipeline": report.pipeline
        })))
    }

    fn observe_stop(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let activation_id = required_string(request, "activation_id")?;
        let reason = request
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("observed stop");
        let input = route_tick_input(self.route_locks()).with_stop_observation(
            self.config.run_id.clone(),
            activation_id.to_string(),
            runtime::StopObservation::new(reason),
        );
        let mut next_driver = self.driver.clone();
        let report = next_driver.tick(input);
        self.commit_runtime_with_publication(next_driver, &report.route_decisions)?;
        let (tmux_allocations, actuation) = self.reconcile_post_commit()?;
        let lifecycle = matches!(
            self.run_status(),
            runtime::RunStatus::Completed | runtime::RunStatus::Stopped
        )
        .then(|| self.finalize_terminal_node_panes("run_terminal"))
        .transpose()?;
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "activation_id": activation_id,
            "run_status": run_status_name(self.run_status()),
            "stop_decisions": stop_decisions_json(&report.stop_decisions),
            "tmux_allocations": tmux_allocations,
            "actuation": actuation.to_json(),
            "lifecycle": lifecycle,
            "pipeline": report.pipeline
        })))
    }

    fn activate_node(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let node_id = required_string(request, "node_id")?;
        let node = node_spec_from_request(request, node_id)?;
        let mut next_driver = self.driver.clone();
        let activation_id = next_driver
            .runtime_mut()
            .activate_node(&self.config.run_id, &node, None)
            .map_err(DriverFailure::from_runtime)?;
        let report = next_driver.tick(route_tick_input(self.route_locks()));
        self.commit_runtime_with_publication(next_driver, &report.route_decisions)?;
        let (tmux_allocations, actuation) = self.reconcile_post_commit()?;
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "activation_id": activation_id,
            "tmux_allocations": tmux_allocations,
            "actuation": actuation.to_json(),
            "pipeline": report.pipeline
        })))
    }

    fn fanout_from_artifact(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let node_id = required_string(request, "node_id")?;
        let node = node_spec_from_request(request, node_id)?;
        let artifact_key = request
            .get("artifact_id")
            .or_else(|| request.get("artifact_key"))
            .or_else(|| request.get("artifactKey"))
            .or_else(|| request.get("key"))
            .and_then(Value::as_str)
            .ok_or_else(|| DriverFailure::new("malformed_request", "artifact id is required"))?;
        let mut next_driver = self.driver.clone();
        let activation_ids = next_driver
            .runtime_mut()
            .fanout_from_artifact(&self.config.run_id, &node, artifact_key)
            .map_err(DriverFailure::from_runtime)?;
        let report = next_driver.tick(route_tick_input(self.route_locks()));
        self.commit_runtime_with_publication(next_driver, &report.route_decisions)?;
        let (tmux_allocations, actuation) = self.reconcile_post_commit()?;
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "node_id": node_id,
            "artifact_key": artifact_key,
            "activation_ids": activation_ids,
            "tmux_allocations": tmux_allocations,
            "actuation": actuation.to_json(),
            "pipeline": report.pipeline
        })))
    }

    fn apply_flow_revision(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let package = StoredFlowRevision::from_request(request, &self.review_store)?;
        let lock_id = package.lock_id().to_string();
        let content_hash = package.content_hash().to_string();
        let mode = flow_lock_mode_from_request(request)?;
        self.write_lock_revision(&lock_id, &content_hash, &package)?;
        let lock = package.lock()?;
        let mut next_driver = self.driver.clone();
        next_driver
            .runtime_mut()
            .apply_flow_lock(
                self.config.run_id.clone(),
                mode,
                lock_id.clone(),
                content_hash.clone(),
            )
            .map_err(DriverFailure::from_runtime)?;
        let mut route_locks = self.route_locks();
        route_locks.retain(|existing| existing.id() != lock.id());
        route_locks.push(lock);
        let report = next_driver.tick(route_tick_input(route_locks));
        self.commit_runtime_with_publication(next_driver, &report.route_decisions)?;
        self.publish_applied_lock_revision(&lock_id, &content_hash, &package)?;
        self.publish_run_asset_flow_revision(&package)?;
        let (tmux_allocations, actuation) = self.reconcile_post_commit()?;
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "flow_lock_id": lock_id,
            "content_hash": content_hash,
            "apply_mode": flow_lock_mode_name(mode),
            "tmux_allocations": tmux_allocations,
            "actuation": actuation.to_json(),
            "pipeline": report.pipeline
        })))
    }

    fn route_locks(&self) -> Vec<flow::FlowLock> {
        self.locks
            .values()
            .filter_map(|package| package.lock().ok())
            .collect()
    }

    fn has_bound_run(&self) -> bool {
        let state = self.driver.runtime().state();
        let Some(lock_id) = state.flow_lock_id_by_run.get(&self.config.run_id) else {
            return false;
        };
        let Some(content_hash) = state.contract_hash_by_run.get(&self.config.run_id) else {
            return false;
        };
        self.locks
            .get(lock_id)
            .is_some_and(|package| package.content_hash() == content_hash && package.lock().is_ok())
    }

    fn console_status(&self) -> String {
        format!(
            "run_id={} run_status={}",
            self.config.run_id,
            run_status_name(self.run_status())
        )
    }

    fn console_why(&self) -> String {
        format!(
            "run_id={} cause=run is {}",
            self.config.run_id,
            run_status_name(self.run_status())
        )
    }

    fn console_control(&mut self, command: &str) -> String {
        let control = match command {
            "pause" => ControlCommand::PauseRun {
                run_id: self.config.run_id.clone(),
            },
            "resume" => ControlCommand::ResumeRun {
                run_id: self.config.run_id.clone(),
            },
            "complete" => ControlCommand::CompleteRun {
                run_id: self.config.run_id.clone(),
            },
            "stop" => ControlCommand::StopRun {
                run_id: self.config.run_id.clone(),
            },
            _ => {
                return "unknown command; try help".into();
            }
        };
        match self.control(control, &Value::Null) {
            Ok(_) => self.console_status(),
            Err(error) => format!("run_id={} error={}", self.config.run_id, error.message),
        }
    }

    fn console_activations(&self) -> String {
        let activations = self
            .driver
            .runtime()
            .state()
            .activations
            .values()
            .filter(|activation| activation.run_id == self.config.run_id)
            .map(|activation| {
                format!(
                    "{}:{}",
                    activation.activation_id,
                    activation_status_name(activation.status)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("run_id={} activations={activations}", self.config.run_id)
    }

    fn console_revisions(&self) -> String {
        let revisions = self
            .driver
            .runtime()
            .state()
            .flow_lock_applications
            .values()
            .filter(|application| application.run_id == self.config.run_id)
            .map(|application| application.application_id.clone())
            .collect::<Vec<_>>()
            .join(",");
        format!("run_id={} revisions={revisions}", self.config.run_id)
    }

    fn run_status(&self) -> runtime::RunStatus {
        self.driver
            .runtime()
            .state()
            .run_status(&self.config.run_id)
            .unwrap_or(runtime::RunStatus::PendingReview)
    }

    fn event_cursor(&self) -> u64 {
        self.driver
            .runtime()
            .events()
            .last()
            .map(|event| event.sequence)
            .unwrap_or(0)
    }

    fn context_generation(&self) -> u64 {
        self.event_cursor().saturating_add(self.driver_event_count)
    }

    fn with_authority_fields(&self, mut response: Value) -> Value {
        if let Value::Object(object) = &mut response {
            object.insert("event_cursor".into(), json!(self.event_cursor()));
            object.insert(
                "context_generation".into(),
                json!(self.context_generation()),
            );
        }
        response
    }

    fn check_expected_authority(&self, request: &Value) -> Result<(), DriverFailure> {
        let expected_event_cursor =
            optional_u64_field(request, &["expected_event_cursor", "expectedEventCursor"])?;
        let expected_context_generation = optional_u64_field(
            request,
            &["expected_context_generation", "expectedContextGeneration"],
        )?;
        let actual_event_cursor = self.event_cursor();
        let actual_context_generation = self.context_generation();
        if expected_event_cursor.is_some_and(|expected| expected != actual_event_cursor)
            || expected_context_generation
                .is_some_and(|expected| expected != actual_context_generation)
        {
            let mut failure = DriverFailure::new(
                "conflict",
                "stale driver authority cursor or context generation",
            );
            failure.extra = json!({
                "expected_event_cursor": expected_event_cursor,
                "actual_event_cursor": actual_event_cursor,
                "expected_context_generation": expected_context_generation,
                "actual_context_generation": actual_context_generation
            });
            return Err(failure);
        }
        Ok(())
    }

    fn append_driver_event(&mut self, kind: &str, payload: Value) -> Result<(), DriverFailure> {
        fail_driver_event_append_if_requested(kind)?;
        let next_seq = self.driver_event_count.saturating_add(1);
        let event = DriverDurableEvent {
            seq: next_seq,
            at_ms: unix_time_ms(),
            kind: kind.to_string(),
            payload,
        };
        append_json_line_private(&self.driver_dir().join(DRIVER_EVENTS_FILE), &event)?;
        self.driver_event_count = next_seq;
        Ok(())
    }

    fn driver_dir(&self) -> PathBuf {
        self.private_run_root.join("driver")
    }

    fn events_path(&self) -> PathBuf {
        self.driver_dir().join(EVENTS_FILE)
    }

    fn revisions_dir(&self) -> PathBuf {
        self.driver_dir().join(REVISIONS_DIR)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DriverDurableEvent {
    seq: u64,
    at_ms: u64,
    kind: String,
    payload: Value,
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeEventBatchRecord {
    protocol: &'static str,
    base_event_count: usize,
    events: Vec<runtime::Event>,
}

#[derive(Debug)]
struct DriverFailure {
    code: &'static str,
    message: String,
    extra: Value,
}

impl DriverFailure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            extra: Value::Null,
        }
    }

    fn io(code: &'static str, err: impl std::fmt::Display) -> Self {
        Self::new(code, err.to_string())
    }

    fn from_runtime(err: runtime::RuntimeError) -> Self {
        Self::new("runtime_error", err.to_string())
    }

    fn from_tmux(err: crate::adapters::tmux::TmuxError) -> Self {
        Self::new("tmux_error", err.to_string())
    }

    fn from_run_asset(err: RunAssetError) -> Self {
        let code = if err.is_publication_blocked() {
            "publication_blocked"
        } else {
            "run_asset_error"
        };
        Self::new(code, err.to_string())
    }

    fn with_tmux_cleanup(mut self, panes: Vec<Value>) -> Self {
        if panes.is_empty() {
            return self;
        }
        let mut merged = self.tmux_cleanup_panes();
        for pane in panes {
            if !merged.contains(&pane) {
                merged.push(pane);
            }
        }
        let extra = match &mut self.extra {
            Value::Object(extra) => extra,
            _ => {
                self.extra = Value::Object(Map::new());
                self.extra.as_object_mut().expect("extra object")
            }
        };
        extra.insert(
            "tmux_cleanup".into(),
            json!({
                "panes": merged
            }),
        );
        self
    }

    fn replacing_tmux_cleanup(mut self, panes: Vec<Value>) -> Self {
        if let Value::Object(extra) = &mut self.extra {
            extra.remove("tmux_cleanup");
        }
        self.with_tmux_cleanup(panes)
    }

    fn with_tmux_release_persistence_failure(mut self, pane: Value) -> Self {
        let mut outcomes = self.tmux_release_outcomes();
        let outcome = json!({
            "pane": pane,
            "physical_release": "complete",
            "ownership_persistence": "failed"
        });
        if !outcomes.contains(&outcome) {
            outcomes.push(outcome);
        }
        let extra = match &mut self.extra {
            Value::Object(extra) => extra,
            _ => {
                self.extra = Value::Object(Map::new());
                self.extra.as_object_mut().expect("extra object")
            }
        };
        extra.insert("tmux_release_outcomes".into(), Value::Array(outcomes));
        self
    }

    fn with_tmux_release_outcomes(mut self, outcomes: Vec<Value>) -> Self {
        for outcome in outcomes {
            let Some(pane) = outcome.get("pane") else {
                continue;
            };
            self = self.with_tmux_release_persistence_failure(pane.clone());
        }
        self
    }

    fn tmux_release_outcomes(&self) -> Vec<Value> {
        self.extra
            .get("tmux_release_outcomes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn tmux_cleanup_panes(&self) -> Vec<Value> {
        self.extra
            .get("tmux_cleanup")
            .and_then(|cleanup| cleanup.get("panes"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn to_response(&self, id: Value) -> Value {
        let mut error = json!({
            "code": self.code,
            "message": self.message
        });
        if let (Value::Object(object), Value::Object(extra)) = (&mut error, &self.extra) {
            for (key, value) in extra {
                object.insert(key.clone(), value.clone());
            }
        }
        json!({
            "id": id,
            "ok": false,
            "error": error
        })
    }
}

fn fail_driver_event_append_if_requested(kind: &str) -> Result<(), DriverFailure> {
    let Some(marker) = std::env::var_os("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_IF_EXISTS") else {
        return Ok(());
    };
    let expected_kind = std::env::var("HUMANIZE_DRIVER_FAIL_DRIVER_EVENT_KIND").ok();
    if expected_kind
        .as_deref()
        .is_some_and(|expected| expected != kind)
    {
        return Ok(());
    }
    if PathBuf::from(marker).exists() {
        return Err(DriverFailure::new(
            "persistence_failed",
            format!("injected driver event append failure for {kind}"),
        ));
    }
    Ok(())
}

fn maybe_crash_after_tmux_effect(kind: &str) {
    let Some(marker) = std::env::var_os("HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_IF_EXISTS") else {
        return;
    };
    if std::env::var("HUMANIZE_DRIVER_CRASH_AFTER_TMUX_EFFECT_KIND")
        .ok()
        .as_deref()
        != Some(kind)
    {
        return;
    }
    let marker = PathBuf::from(marker);
    if !marker.exists() {
        return;
    }
    let _ = fs::remove_file(marker);
    std::process::exit(86);
}

impl From<io::Error> for DriverFailure {
    fn from(err: io::Error) -> Self {
        Self::io("io_error", err)
    }
}

fn run_mode_from_request(request: &Value) -> Result<runtime::RunMode, DriverFailure> {
    let Some(mode) = request
        .get("run_mode")
        .or_else(|| request.get("runMode"))
        .and_then(Value::as_str)
    else {
        return Ok(runtime::RunMode::Finite);
    };
    match mode {
        "finite" => Ok(runtime::RunMode::Finite),
        "continuous" => Ok(runtime::RunMode::Continuous),
        "manual" => Ok(runtime::RunMode::Manual),
        value => Err(DriverFailure::new(
            "malformed_request",
            format!("unknown run mode: {value}"),
        )),
    }
}

fn activation_limit_from_request(request: &Value) -> Result<u64, DriverFailure> {
    optional_u64_field(request, &["activation_limit", "activationLimit"])
        .map(|limit| limit.unwrap_or(u64::MAX))
}

fn stop_attempt_limit_from_request(request: &Value) -> Result<u32, DriverFailure> {
    let limit = optional_u64_field(request, &["stop_attempt_limit", "stopAttemptLimit"])?
        .unwrap_or(participant::DEFAULT_STOP_ATTEMPT_LIMIT.into());
    let limit = u32::try_from(limit).map_err(|_| {
        DriverFailure::new("malformed_request", "stop_attempt_limit is out of range")
    })?;
    if !(1..=participant::MAX_STOP_ATTEMPT_LIMIT).contains(&limit) {
        return Err(DriverFailure::new(
            "malformed_request",
            format!(
                "stop_attempt_limit must be between 1 and {}",
                participant::MAX_STOP_ATTEMPT_LIMIT
            ),
        ));
    }
    Ok(limit)
}

fn flow_lock_mode_from_request(request: &Value) -> Result<runtime::FlowLockMode, DriverFailure> {
    let Some(mode) = request
        .get("mode")
        .or_else(|| request.get("apply_mode"))
        .or_else(|| request.get("applyMode"))
        .and_then(Value::as_str)
    else {
        return Ok(runtime::FlowLockMode::FutureActivations);
    };
    match mode {
        "future_activations" | "futureActivations" | "future-activations" => {
            Ok(runtime::FlowLockMode::FutureActivations)
        }
        "checkpoint_restart" | "checkpointRestart" | "checkpoint-restart" => {
            Ok(runtime::FlowLockMode::CheckpointRestart)
        }
        value => Err(DriverFailure::new(
            "malformed_request",
            format!("unknown flow lock mode: {value}"),
        )),
    }
}

fn initial_node_specs(draft: &flow::FlowDraft) -> Vec<NodeSpec> {
    let route_targets = draft
        .routes
        .iter()
        .map(|route| route.activate.as_str())
        .collect::<BTreeSet<_>>();
    let contracts = flow::NodeContract::from_draft(draft);
    let initial = contracts
        .iter()
        .filter(|contract| !route_targets.contains(contract.node_id.as_str()))
        .map(node_spec_from_contract)
        .collect::<Vec<_>>();
    if initial.is_empty() {
        contracts.iter().map(node_spec_from_contract).collect()
    } else {
        initial
    }
}

fn node_spec_from_contract(contract: &flow::NodeContract) -> NodeSpec {
    let required_artifacts = contract
        .artifact_requirements
        .iter()
        .filter(|artifact| artifact.required)
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    let required_effects = contract
        .effect_requirements
        .iter()
        .filter(|effect| effect.required)
        .map(|effect| effect.id.clone())
        .collect::<Vec<_>>();
    NodeSpec::new(&contract.node_id)
        .with_stop_contract(StopContract::new(required_artifacts, required_effects))
}

fn node_spec_from_request(request: &Value, node_id: &str) -> Result<NodeSpec, DriverFailure> {
    let Some(value) = request.get("node_spec") else {
        return Ok(NodeSpec::new(node_id));
    };
    let node = serde_json::from_value::<NodeSpec>(value.clone())
        .map_err(|err| DriverFailure::new("malformed_request", err.to_string()))?;
    if node.id() != node_id {
        return Err(DriverFailure::new(
            "malformed_request",
            "node_spec id does not match node_id",
        ));
    }
    Ok(node)
}

fn route_tick_input(route_locks: Vec<flow::FlowLock>) -> DriverTickInput {
    route_locks
        .into_iter()
        .fold(DriverTickInput::default(), |input, lock| {
            input.with_route_lock(lock)
        })
}
