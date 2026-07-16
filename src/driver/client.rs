use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::ipc::{
    FrameRead, IO_TIMEOUT, MAX_FRAME_BYTES, connect_with_timeout, read_frame_with_timeout,
    write_bytes,
};
use super::storage::{atomic_write_private_json_io, unix_time_ms};
use super::{DriverConfig, IPC_FILE, socket_path_for_run_root};

const TOKEN_FILE: &str = "ipc-token";
const MUTATION_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverIpcMetadata {
    pub run_id: String,
    pub socket_path: PathBuf,
    pub auth_token_path: PathBuf,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct DriverClient {
    metadata: DriverIpcMetadata,
    socket_path: PathBuf,
    auth_token: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum DriverEndpointState {
    Accepting,
    Dead,
}

impl DriverClient {
    pub fn from_run_root(run_root: &Path) -> io::Result<Option<Self>> {
        let driver_dir = private_driver_dir_for_run_root(run_root)?;
        let metadata_path = driver_dir.join(IPC_FILE);
        if !metadata_path.exists() {
            return Ok(None);
        }
        let run_id = private_run_id(run_root)?;
        Self::from_run_root_for_run(run_root, &run_id)
    }

    pub fn from_run_root_for_run(run_root: &Path, run_id: &str) -> io::Result<Option<Self>> {
        let driver_dir = private_driver_dir_for_run_root(run_root)?;
        let metadata_path = driver_dir.join(IPC_FILE);
        if !metadata_path.exists() {
            return Ok(None);
        }
        if private_run_id(run_root)? != run_id {
            return Err(invalid_data("private run identity mismatch"));
        }
        validate_private_dir(&driver_dir, "driver directory")?;
        let metadata_bytes = crate::run_assets::read_regular_private(&metadata_path)
            .map_err(|err| invalid_data(err.to_string()))?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "driver IPC metadata is missing")
            })?;
        let metadata = serde_json::from_slice::<DriverIpcMetadata>(&metadata_bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        if metadata.run_id != run_id {
            return Err(invalid_data("driver IPC metadata run identity mismatch"));
        }

        let runtime_root = runtime_root_for_run_root(run_root)?;
        validate_private_dir(&runtime_root, "driver runtime directory")?;
        let expected_socket = socket_path_for_run_root(&runtime_root, run_root)?;
        validate_relative_identity(
            &metadata.socket_path,
            expected_socket
                .file_name()
                .unwrap_or_else(|| OsStr::new("")),
            "driver IPC socket path",
        )?;
        validate_private_socket(&expected_socket)?;

        validate_relative_identity(
            &metadata.auth_token_path,
            OsStr::new(TOKEN_FILE),
            "driver IPC token path",
        )?;
        let token_path = driver_dir.join(TOKEN_FILE);
        let token_bytes = crate::run_assets::read_regular_private(&token_path)
            .map_err(|err| invalid_data(err.to_string()))?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "driver IPC token is missing")
            })?;
        let auth_token = std::str::from_utf8(&token_bytes)
            .map_err(|_| invalid_data("driver IPC token is not UTF-8"))?
            .trim()
            .to_string();
        if auth_token.is_empty() {
            return Err(invalid_data("driver IPC token file is empty"));
        }
        Ok(Some(Self {
            metadata,
            socket_path: expected_socket,
            auth_token,
        }))
    }

    pub fn request(&self, op: &str, run_id: &str, arguments: &Value) -> io::Result<Value> {
        let request = self.prepare_request(op, run_id, arguments)?;
        self.send_prepared_request(&request)
            .map_err(DriverRequestAttemptError::into_io_error)
    }

    pub(crate) fn request_participant_stop_with_one_ambiguous_retry(
        &self,
        run_id: &str,
        arguments: &Value,
    ) -> io::Result<Value> {
        let request = self.prepare_request("participant_stop", run_id, arguments)?;
        match self.send_prepared_request(&request) {
            Ok(response) => Ok(response),
            Err(DriverRequestAttemptError::Ambiguous(_)) => self
                .send_prepared_request(&request)
                .map_err(DriverRequestAttemptError::into_io_error),
            Err(err) => Err(err.into_io_error()),
        }
    }

    fn prepare_request(
        &self,
        op: &str,
        run_id: &str,
        arguments: &Value,
    ) -> io::Result<PreparedDriverRequest> {
        if run_id != self.metadata.run_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "driver client request run identity mismatch",
            ));
        }
        let mut request = match arguments {
            Value::Object(object) => object.clone(),
            _ => Map::new(),
        };
        request.insert("id".into(), json!("mcp"));
        request.insert("token".into(), json!(self.auth_token));
        request.insert("op".into(), json!(op));
        request.insert("run_id".into(), json!(run_id));
        let mut bytes = serde_json::to_vec(&Value::Object(request)).map_err(io::Error::other)?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "driver IPC request exceeds the maximum frame size",
            ));
        }
        bytes.push(b'\n');
        Ok(PreparedDriverRequest {
            bytes,
            response_timeout: response_timeout(op),
        })
    }

    fn send_prepared_request(
        &self,
        request: &PreparedDriverRequest,
    ) -> Result<Value, DriverRequestAttemptError> {
        let mut stream = connect_with_timeout(&self.socket_path, IO_TIMEOUT)
            .map_err(DriverRequestAttemptError::Definitive)?;
        stream
            .set_read_timeout(Some(request.response_timeout))
            .map_err(DriverRequestAttemptError::Definitive)?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(DriverRequestAttemptError::Definitive)?;
        write_request_bytes(&mut stream, &request.bytes)
            .map_err(DriverRequestAttemptError::Definitive)?;
        let response = match read_frame_with_timeout(&mut stream, request.response_timeout) {
            Err(err) if ambiguous_response_error(&err) => {
                return Err(DriverRequestAttemptError::Ambiguous(err));
            }
            Err(err) => return Err(DriverRequestAttemptError::Definitive(err)),
            Ok(FrameRead::Complete(frame)) => frame,
            Ok(FrameRead::TooLarge) => {
                return Err(DriverRequestAttemptError::Definitive(invalid_data(
                    "driver IPC response exceeds the maximum frame size",
                )));
            }
            Ok(FrameRead::Truncated) => {
                return Err(DriverRequestAttemptError::Ambiguous(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "driver IPC response was truncated",
                )));
            }
            Ok(FrameRead::TimedOut) => {
                return Err(DriverRequestAttemptError::Ambiguous(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "driver IPC response read deadline exceeded",
                )));
            }
        };
        let response = serde_json::from_slice::<Value>(&response).map_err(|err| {
            DriverRequestAttemptError::Definitive(io::Error::new(io::ErrorKind::InvalidData, err))
        })?;
        if response
            .get("response_ack_required")
            .and_then(Value::as_bool)
            == Some(true)
        {
            write_request_bytes(&mut stream, b"{\"ack\":\"response_received\"}\n")
                .map_err(DriverRequestAttemptError::Definitive)?;
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
        Ok(response)
    }
}

struct PreparedDriverRequest {
    bytes: Vec<u8>,
    response_timeout: std::time::Duration,
}

enum DriverRequestAttemptError {
    Definitive(io::Error),
    Ambiguous(io::Error),
}

impl DriverRequestAttemptError {
    fn into_io_error(self) -> io::Error {
        match self {
            Self::Definitive(err) | Self::Ambiguous(err) => err,
        }
    }
}

fn ambiguous_response_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::TimedOut
    )
}

pub(super) fn write_ipc_metadata(config: &DriverConfig) -> io::Result<()> {
    let run_root = config.run_root()?;
    let private_run_root =
        crate::private_state::ensure_private_run_root(&config.runtime_root, &run_root)?;
    let driver_dir = private_run_root.join("driver");
    crate::private_state::ensure_private_directory(&driver_dir)?;
    let token_path = config
        .auth_token_path
        .clone()
        .unwrap_or_else(|| driver_dir.join(TOKEN_FILE));
    if token_path != driver_dir.join(TOKEN_FILE) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "driver IPC token path must be the private run token file",
        ));
    }
    if !token_path.exists() {
        crate::run_assets::write_create_new_private(
            &token_path,
            format!("{}\n", config.auth_token).as_bytes(),
        )
        .map_err(|err| io::Error::other(err.to_string()))?;
    } else {
        let token_bytes = crate::run_assets::read_regular_private(&token_path)
            .map_err(|err| invalid_data(err.to_string()))?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "driver IPC token is missing")
            })?;
        let token = std::str::from_utf8(&token_bytes)
            .map_err(|_| invalid_data("driver IPC token is not UTF-8"))?;
        if token.trim() != config.auth_token {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "driver IPC token file does not match the starting driver",
            ));
        }
    }
    let socket_path = config.socket_path()?;
    let socket_name = socket_path
        .file_name()
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid socket path"))?;
    let metadata = DriverIpcMetadata {
        run_id: config.run_id.clone(),
        socket_path: socket_name,
        auth_token_path: PathBuf::from(TOKEN_FILE),
        updated_at_ms: unix_time_ms(),
    };
    let path = driver_dir.join(IPC_FILE);
    atomic_write_private_json_io(&path, &metadata)
}

pub(crate) fn runtime_root_for_run_root(run_root: &Path) -> io::Result<PathBuf> {
    let _ = run_root;
    crate::state_path::private_runtime_root()
}

pub(crate) fn private_run_root_for_run_root(
    runtime_root: &Path,
    run_root: &Path,
) -> io::Result<PathBuf> {
    crate::private_state::private_run_root(runtime_root, run_root)
}

pub(crate) fn private_driver_dir(runtime_root: &Path, run_root: &Path) -> io::Result<PathBuf> {
    Ok(private_run_root_for_run_root(runtime_root, run_root)?.join("driver"))
}

fn private_driver_dir_for_run_root(run_root: &Path) -> io::Result<PathBuf> {
    let runtime_root = runtime_root_for_run_root(run_root)?;
    private_driver_dir(&runtime_root, run_root)
}

pub fn cleanup_stale_driver_ipc(run_root: &Path, run_id: &str) -> io::Result<()> {
    if private_run_id(run_root)? != run_id {
        return Err(invalid_data("stale driver IPC run identity mismatch"));
    }
    let runtime_root = runtime_root_for_run_root(run_root)?;
    let driver_dir = private_driver_dir(&runtime_root, run_root)?;
    validate_private_dir(&driver_dir, "driver directory")?;
    validate_private_dir(&runtime_root, "driver runtime directory")?;
    let socket_path = socket_path_for_run_root(&runtime_root, run_root)?;
    match fs::symlink_metadata(&socket_path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(invalid_data(
                    "stale driver socket owner is not the current user",
                ));
            }
            match connect_with_timeout(&socket_path, IO_TIMEOUT) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "driver IPC socket still accepts connections",
                    ));
                }
                Err(err)
                    if !matches!(
                        err.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                    ) =>
                {
                    return Err(io::Error::new(
                        err.kind(),
                        format!("driver IPC socket liveness is indeterminate: {err}"),
                    ));
                }
                Err(_) => {}
            }
            fs::remove_file(&socket_path)?;
            fs::File::open(&runtime_root)?.sync_all()?;
        }
        Ok(_) => {
            return Err(invalid_data(
                "stale driver IPC endpoint is not a socket and was not removed",
            ));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    remove_owned_regular_file(&driver_dir.join(IPC_FILE))?;
    remove_owned_regular_file(&driver_dir.join(TOKEN_FILE))?;
    fs::File::open(driver_dir)?.sync_all()
}

pub(crate) fn probe_driver_endpoint(run_root: &Path) -> io::Result<DriverEndpointState> {
    let runtime_root = runtime_root_for_run_root(run_root)?;
    let socket_path = socket_path_for_run_root(&runtime_root, run_root)?;
    match fs::symlink_metadata(&socket_path) {
        Ok(_) => {
            validate_private_dir(&runtime_root, "driver runtime directory")?;
            validate_private_socket(&socket_path)?;
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(DriverEndpointState::Dead);
        }
        Err(err) => return Err(err),
    }
    match connect_with_timeout(&socket_path, IO_TIMEOUT) {
        Ok(stream) => {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            Ok(DriverEndpointState::Accepting)
        }
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            Ok(DriverEndpointState::Dead)
        }
        Err(err) => Err(io::Error::new(
            err.kind(),
            format!("driver IPC endpoint liveness is indeterminate: {err}"),
        )),
    }
}

fn private_run_id(run_root: &Path) -> io::Result<String> {
    let runtime_root = runtime_root_for_run_root(run_root)?;
    crate::private_state::read_run_identity(&runtime_root, run_root)?
        .map(|identity| identity.run_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "private run identity is missing"))
}

fn remove_owned_regular_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(invalid_data(format!(
                    "refusing to remove driver artifact owned by another user at {}",
                    path.display()
                )));
            }
            if metadata.nlink() != 1 {
                return Err(invalid_data(format!(
                    "refusing to remove multiply linked driver artifact at {}",
                    path.display()
                )));
            }
            fs::remove_file(path)
        }
        Ok(_) => Err(invalid_data(format!(
            "refusing to remove non-file driver artifact at {}",
            path.display()
        ))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn validate_relative_identity(path: &Path, expected: &OsStr, label: &str) -> io::Result<()> {
    let mut components = path.components();
    let valid = matches!(components.next(), Some(Component::Normal(value)) if value == expected)
        && components.next().is_none();
    if valid {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "{label} does not match the run endpoint"
        )))
    }
}

fn validate_private_dir(path: &Path, label: &str) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(invalid_data(format!("{label} is not a directory")));
    }
    validate_owner_and_mode(&metadata, 0o700, label)
}

fn validate_private_socket(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err(invalid_data("driver IPC endpoint is not a socket"));
    }
    validate_owner_and_mode(&metadata, 0o600, "driver IPC socket")
}

fn validate_owner_and_mode(
    metadata: &fs::Metadata,
    expected_mode: u32,
    label: &str,
) -> io::Result<()> {
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(invalid_data(format!(
            "{label} owner is not the current user"
        )));
    }
    let actual_mode = metadata.mode() & 0o777;
    if actual_mode != expected_mode {
        return Err(invalid_data(format!(
            "{label} permissions must be {expected_mode:o}, found {actual_mode:o}"
        )));
    }
    Ok(())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn response_timeout(op: &str) -> std::time::Duration {
    if matches!(op, "status" | "context" | "why") {
        IO_TIMEOUT
    } else {
        MUTATION_RESPONSE_TIMEOUT
    }
}

fn write_request_bytes(stream: &mut UnixStream, bytes: &[u8]) -> io::Result<()> {
    write_bytes(stream, bytes)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use super::{DriverClient, DriverIpcMetadata, write_request_bytes};

    #[test]
    fn write_request_bytes_reports_deadline_when_send_queue_is_full() {
        let (mut writer, _reader) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        let block = [0u8; 16 * 1024];
        loop {
            match writer.write(&block) {
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => panic!("failed to fill socket send queue: {err}"),
            }
        }
        writer.set_nonblocking(false).unwrap();
        writer
            .set_write_timeout(Some(Duration::from_millis(50)))
            .unwrap();

        let error = write_request_bytes(&mut writer, b"x").unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("write deadline"));
    }

    #[test]
    fn participant_stop_retries_one_ambiguous_response_with_identical_request() {
        let root = std::env::temp_dir().join(format!(
            "humanize-driver-client-retry-{}",
            std::process::id()
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        let socket_path = root.join("driver.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let server_requests = Arc::clone(&requests);
        let server = thread::spawn(move || {
            let mut decisions = BTreeMap::<String, (u64, &'static str)>::new();
            let mut attempts = 0_u64;
            for connection_index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request = serde_json::from_str::<Value>(line.trim()).unwrap();
                server_requests.lock().unwrap().push(line);
                let invocation_id = request["invocation_id"].as_str().unwrap().to_string();
                let decision = decisions.entry(invocation_id.clone()).or_insert_with(|| {
                    attempts += 1;
                    (attempts, if attempts == 1 { "deny" } else { "block" })
                });
                if connection_index == 0 {
                    continue;
                }
                writeln!(
                    stream,
                    "{}",
                    json!({
                        "id": request["id"],
                        "ok": true,
                        "invocation_id": invocation_id,
                        "attempt": decision.0,
                        "decision": decision.1
                    })
                )
                .unwrap();
            }
            attempts
        });
        let client = DriverClient {
            metadata: DriverIpcMetadata {
                run_id: "run-stop-retry".to_string(),
                socket_path: socket_path.clone(),
                auth_token_path: "ipc-token".into(),
                updated_at_ms: 0,
            },
            socket_path,
            auth_token: "test-token".to_string(),
        };
        let first = client
            .request_participant_stop_with_one_ambiguous_retry(
                "run-stop-retry",
                &json!({
                    "activation_id": "root",
                    "invocation_id": "stop-invocation-a",
                    "reason": "participant requested stop"
                }),
            )
            .unwrap();
        assert_eq!(first["attempt"], 1);
        assert_eq!(first["decision"], "deny");

        let second = client
            .request_participant_stop_with_one_ambiguous_retry(
                "run-stop-retry",
                &json!({
                    "activation_id": "root",
                    "invocation_id": "stop-invocation-b",
                    "reason": "participant requested stop"
                }),
            )
            .unwrap();
        assert_eq!(second["attempt"], 2);
        assert_eq!(second["decision"], "block");
        assert_eq!(server.join().unwrap(), 2);

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0], requests[1]);
        let requests = requests
            .iter()
            .map(|request| serde_json::from_str::<Value>(request.trim()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(requests[0]["id"], requests[1]["id"]);
        assert_eq!(requests[0]["invocation_id"], requests[1]["invocation_id"]);
        assert_ne!(requests[1]["invocation_id"], requests[2]["invocation_id"]);
        drop(requests);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ordinary_request_does_not_retry_an_ambiguous_response() {
        let root = std::env::temp_dir().join(format!(
            "humanize-driver-client-single-attempt-{}",
            std::process::id()
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        let socket_path = root.join("driver.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            drop(stream);
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_millis(200);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok(_) => return true,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => panic!("accept failed: {err}"),
                }
            }
            false
        });
        let client = DriverClient {
            metadata: DriverIpcMetadata {
                run_id: "run-single-attempt".to_string(),
                socket_path: socket_path.clone(),
                auth_token_path: "ipc-token".into(),
                updated_at_ms: 0,
            },
            socket_path,
            auth_token: "test-token".to_string(),
        };
        let error = client
            .request(
                "deliver_artifact",
                "run-single-attempt",
                &json!({
                    "activation_id": "root",
                    "artifact_id": "brief",
                    "payload": "done"
                }),
            )
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
        assert!(!server.join().unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
