use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use humanize_plugin::driver::{DriverClient, cleanup_stale_driver_ipc};
use humanize_plugin::run_assets::{RunAssetSink, RunAssetStore};
use serde_json::{Value, json};

const MAX_FRAME_BYTES: usize = 1024 * 1024;
static STATE_ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn idle_client_does_not_block_an_authenticated_request() {
    let fixture = DriverFixture::new("idle-client");
    let mut driver = fixture.spawn("run-idle");
    let idle = UnixStream::connect(fixture.socket_path("run-idle")).unwrap();

    let started = Instant::now();
    let response = fixture.request(
        "run-idle",
        json!({
            "id": "status",
            "token": fixture.token,
            "op": "status",
            "run_id": "run-idle"
        }),
    );

    assert_eq!(response["ok"], true, "{response}");
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(idle);
    driver.shutdown();
}

#[test]
fn connection_processes_exactly_one_request_then_closes() {
    let fixture = DriverFixture::new("single-request");
    let mut driver = fixture.spawn("run-single");
    let request = json!({
        "id": "status",
        "token": fixture.token,
        "op": "status",
        "run_id": "run-single"
    });
    let mut stream = UnixStream::connect(fixture.socket_path("run-single")).unwrap();
    stream
        .write_all(format!("{request}\n{request}\n").as_bytes())
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut responses = String::new();
    stream.read_to_string(&mut responses).unwrap();

    assert_eq!(responses.lines().count(), 1, "{responses}");
    assert_eq!(
        serde_json::from_str::<Value>(&responses).unwrap()["ok"],
        true
    );
    driver.shutdown();
}

#[test]
fn oversized_frame_is_rejected_and_closed() {
    let fixture = DriverFixture::new("oversized-frame");
    let mut driver = fixture.spawn("run-oversized");
    let mut stream = UnixStream::connect(fixture.socket_path("run-oversized")).unwrap();
    stream
        .write_all(format!("{}\n", "x".repeat(MAX_FRAME_BYTES + 1)).as_bytes())
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut response = String::new();
    BufReader::new(stream)
        .read_to_string(&mut response)
        .unwrap();
    let response: Value = serde_json::from_str(response.trim()).unwrap();

    assert_eq!(response["error"]["code"], "request_too_large");
    driver.shutdown();
}

#[test]
fn truncated_frame_is_rejected_and_closed() {
    let fixture = DriverFixture::new("truncated-frame");
    let mut driver = fixture.spawn("run-truncated");
    let mut stream = UnixStream::connect(fixture.socket_path("run-truncated")).unwrap();
    stream.write_all(b"{\"id\":\"partial\"").unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut response = String::new();
    BufReader::new(stream)
        .read_to_string(&mut response)
        .unwrap();
    let response: Value = serde_json::from_str(response.trim()).unwrap();

    assert_eq!(response["error"]["code"], "truncated_request");
    driver.shutdown();
}

#[test]
fn idle_connection_is_closed_after_the_server_read_deadline() {
    let fixture = DriverFixture::new("server-read-timeout");
    let mut driver = fixture.spawn("run-server-timeout");
    let mut stream = UnixStream::connect(fixture.socket_path("run-server-timeout")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let started = Instant::now();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    assert!(started.elapsed() < Duration::from_secs(3));
    let response: Value = serde_json::from_str(response.trim()).unwrap();
    assert_eq!(response["error"]["code"], "request_timeout");
    driver.shutdown();
}

#[test]
fn slow_drip_frame_is_closed_at_the_absolute_read_deadline() {
    let fixture = DriverFixture::new("server-absolute-timeout");
    let mut driver = fixture.spawn("run-absolute-timeout");
    let mut stream = UnixStream::connect(fixture.socket_path("run-absolute-timeout")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let started = Instant::now();
    for _ in 0..8 {
        if stream.write_all(b"{").is_err() {
            break;
        }
        thread::sleep(Duration::from_millis(400));
    }
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    assert!(started.elapsed() < Duration::from_secs(3));
    let response: Value = serde_json::from_str(response.trim()).unwrap();
    assert_eq!(response["error"]["code"], "request_timeout");
    driver.shutdown();
}

#[test]
fn client_rejects_socket_path_substitution_and_public_metadata() {
    let fixture = DriverFixture::new("metadata-substitution");
    let mut driver = fixture.spawn("run-metadata");
    let metadata_path = fixture.private_driver_dir("run-metadata").join("ipc.json");
    let mut metadata: Value = serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    metadata["socket_path"] = json!(fixture.root.join("other.sock"));
    fs::write(
        &metadata_path,
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let mut permissions = fs::metadata(&metadata_path).unwrap().permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&metadata_path, permissions).unwrap();

    let error = DriverClient::from_run_root(&fixture.run_root("run-metadata")).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error.to_string().contains("metadata permissions")
            || error.to_string().contains("socket path")
            || error.to_string().contains("current-user mode 600"),
        "{error}"
    );
    driver.shutdown();
}

#[test]
fn client_read_deadline_bounds_a_nonresponding_same_user_peer() {
    let fixture = DriverFixture::new("client-read-timeout");
    let mut driver = fixture.spawn("run-client-timeout");
    let socket_path = fixture.socket_path("run-client-timeout");
    driver.crash();
    fs::remove_file(&socket_path).unwrap();
    let listener = UnixListener::bind(&socket_path).unwrap();
    let mut permissions = fs::metadata(&socket_path).unwrap().permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&socket_path, permissions).unwrap();
    let peer = thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_secs(3));
    });
    let client = DriverClient::from_run_root_for_run(
        &fixture.run_root("run-client-timeout"),
        "run-client-timeout",
    )
    .unwrap()
    .unwrap();

    let started = Instant::now();
    let error = client
        .request("status", "run-client-timeout", &json!({}))
        .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(3));
    peer.join().unwrap();
}

#[test]
fn stale_cleanup_ignores_metadata_socket_substitution() {
    let fixture = DriverFixture::new("cleanup-substitution");
    let mut driver = fixture.spawn("run-cleanup-substitution");
    driver.crash();
    let outside_socket = fixture.root.join("outside.sock");
    let outside_listener = UnixListener::bind(&outside_socket).unwrap();
    let metadata_path = fixture
        .private_driver_dir("run-cleanup-substitution")
        .join("ipc.json");
    let mut metadata: Value = serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    metadata["socket_path"] = json!(outside_socket);
    fs::write(
        &metadata_path,
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let mut permissions = fs::metadata(&metadata_path).unwrap().permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&metadata_path, permissions).unwrap();

    cleanup_stale_driver_ipc(
        &fixture.run_root("run-cleanup-substitution"),
        "run-cleanup-substitution",
    )
    .unwrap();

    assert!(outside_socket.exists());
    assert!(!metadata_path.exists());
    drop(outside_listener);
}

#[test]
fn client_validates_run_token_parent_and_socket_identity() {
    let fixture = DriverFixture::new("metadata-identity");
    let mut driver = fixture.spawn("run-identity");
    let run_root = fixture.run_root("run-identity");
    let metadata_path = fixture.private_driver_dir("run-identity").join("ipc.json");
    let original = fs::read(&metadata_path).unwrap();
    let mut metadata: Value = serde_json::from_slice(&original).unwrap();

    metadata["run_id"] = json!("other-run");
    write_private_json(&metadata_path, &metadata);
    let error = DriverClient::from_run_root_for_run(&run_root, "run-identity").unwrap_err();
    assert!(error.to_string().contains("run identity"), "{error}");

    metadata = serde_json::from_slice(&original).unwrap();
    metadata["auth_token_path"] = json!("../ipc-token");
    write_private_json(&metadata_path, &metadata);
    let error = DriverClient::from_run_root_for_run(&run_root, "run-identity").unwrap_err();
    assert!(error.to_string().contains("token path"), "{error}");

    fs::write(&metadata_path, &original).unwrap();
    let runtime_root = fixture.root.join("runtime");
    let mut permissions = fs::metadata(&runtime_root).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&runtime_root, permissions).unwrap();
    let error = DriverClient::from_run_root_for_run(&run_root, "run-identity").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("private runtime root permissions must be 700, found 755"),
        "{error}"
    );

    let mut permissions = fs::metadata(&runtime_root).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&runtime_root, permissions).unwrap();
    driver.crash();
    let socket_path = fixture.socket_path("run-identity");
    fs::remove_file(&socket_path).unwrap();
    fs::write(&socket_path, "not a socket").unwrap();
    let mut permissions = fs::metadata(&socket_path).unwrap().permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&socket_path, permissions).unwrap();
    let error = DriverClient::from_run_root_for_run(&run_root, "run-identity").unwrap_err();
    assert!(error.to_string().contains("not a socket"), "{error}");
}

#[test]
fn client_rejects_hardlinked_private_metadata_and_token() {
    let fixture = DriverFixture::new("private-hardlinks");
    let mut driver = fixture.spawn("run-private-hardlinks");
    let run_root = fixture.run_root("run-private-hardlinks");
    let driver_dir = fixture.private_driver_dir("run-private-hardlinks");

    let metadata_path = driver_dir.join("ipc.json");
    let metadata_copy = fixture.root.join("outside-ipc.json");
    fs::copy(&metadata_path, &metadata_copy).unwrap();
    fs::set_permissions(&metadata_copy, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&metadata_path).unwrap();
    fs::hard_link(&metadata_copy, &metadata_path).unwrap();
    let error =
        DriverClient::from_run_root_for_run(&run_root, "run-private-hardlinks").unwrap_err();
    assert!(error.to_string().contains("exactly one link"), "{error}");

    fs::remove_file(&metadata_path).unwrap();
    fs::copy(&metadata_copy, &metadata_path).unwrap();
    fs::set_permissions(&metadata_path, fs::Permissions::from_mode(0o600)).unwrap();
    let token_path = driver_dir.join("ipc-token");
    let token_copy = fixture.root.join("outside-ipc-token");
    fs::copy(&token_path, &token_copy).unwrap();
    fs::set_permissions(&token_copy, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&token_path).unwrap();
    fs::hard_link(&token_copy, &token_path).unwrap();
    let error =
        DriverClient::from_run_root_for_run(&run_root, "run-private-hardlinks").unwrap_err();
    assert!(error.to_string().contains("exactly one link"), "{error}");
    driver.crash();
}

#[test]
fn client_rejects_fifo_private_token_without_blocking() {
    let fixture = DriverFixture::new("private-token-fifo");
    let mut driver = fixture.spawn("run-private-token-fifo");
    let run_root = fixture.run_root("run-private-token-fifo");
    let token_path = fixture
        .private_driver_dir("run-private-token-fifo")
        .join("ipc-token");
    fs::remove_file(&token_path).unwrap();
    let token = std::ffi::CString::new(token_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(token.as_ptr(), 0o600) }, 0);

    let started = Instant::now();
    let error =
        DriverClient::from_run_root_for_run(&run_root, "run-private-token-fifo").unwrap_err();

    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(error.to_string().contains("not a regular file"), "{error}");
    driver.crash();
}

#[test]
fn fixture_drop_removes_stale_socket_files_after_driver_crash() {
    let root = {
        let fixture = DriverFixture::new("fixture-stale-socket-cleanup");
        let mut driver = fixture.spawn("run-fixture-stale-socket-cleanup");
        let socket_path = fixture.socket_path("run-fixture-stale-socket-cleanup");
        driver.crash();
        assert!(socket_path.exists());
        fixture.root.clone()
    };

    assert!(
        !root.exists(),
        "fixture root remained at {}",
        root.display()
    );
}

struct DriverFixture {
    root: PathBuf,
    token: &'static str,
    prior_state_root: Option<OsString>,
    _state_env_guard: MutexGuard<'static, ()>,
}

impl DriverFixture {
    fn new(name: &str) -> Self {
        let state_env_guard = STATE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let root = std::env::temp_dir()
            .join("humanize-plugin-driver-ipc-bounds")
            .join(format!("{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(root.join("runtime")).unwrap();
        fs::create_dir_all(root.join("runs")).unwrap();
        let prior_state_root = std::env::var_os("HUMANIZE_STATE_ROOT");
        unsafe {
            std::env::set_var("HUMANIZE_STATE_ROOT", &root);
        }
        Self {
            root,
            token: "test-token",
            prior_state_root,
            _state_env_guard: state_env_guard,
        }
    }

    fn spawn(&self, run_id: &str) -> DriverProcess {
        let mut child = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-driver"))
            .arg("--run-id")
            .arg(run_id)
            .arg("--runs-root")
            .arg(self.root.join("runs"))
            .arg("--runtime-root")
            .arg(self.root.join("runtime"))
            .arg("--auth-token")
            .arg(self.token)
            .env("HUMANIZE_STATE_ROOT", &self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_for_metadata(
            &mut child,
            &self.private_driver_dir(run_id).join("ipc.json"),
        );
        DriverProcess { child }
    }

    fn run_root(&self, run_id: &str) -> PathBuf {
        RunAssetStore::new(RunAssetSink::HumanizeRunsDir(self.root.join("runs")))
            .run_root(run_id)
            .unwrap()
    }

    fn private_run_root(&self, run_id: &str) -> PathBuf {
        let run_root = self.run_root(run_id);
        let identity = std::path::absolute(&run_root)
            .unwrap_or(run_root)
            .to_string_lossy()
            .into_owned();
        self.root
            .join("runtime")
            .join(format!("r{:016x}", stable_hash(&identity)))
    }

    fn private_driver_dir(&self, run_id: &str) -> PathBuf {
        self.private_run_root(run_id).join("driver")
    }

    fn socket_path(&self, run_id: &str) -> PathBuf {
        let metadata: Value = serde_json::from_slice(
            &fs::read(self.private_driver_dir(run_id).join("ipc.json")).unwrap(),
        )
        .unwrap();
        let path = PathBuf::from(metadata["socket_path"].as_str().unwrap());
        if path.is_absolute() {
            path
        } else {
            self.private_run_root(run_id).join(path)
        }
    }

    fn request(&self, run_id: &str, request: Value) -> Value {
        let mut stream = UnixStream::connect(self.socket_path(run_id)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(format!("{request}\n").as_bytes()).unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    }
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

impl Drop for DriverFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        unsafe {
            match self.prior_state_root.take() {
                Some(value) => std::env::set_var("HUMANIZE_STATE_ROOT", value),
                None => std::env::remove_var("HUMANIZE_STATE_ROOT"),
            }
        }
    }
}

struct DriverProcess {
    child: Child,
}

impl DriverProcess {
    fn shutdown(&mut self) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, "quit");
            let _ = stdin.flush();
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(3) {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn crash(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

impl Drop for DriverProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn wait_for_metadata(child: &mut Child, path: &Path) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if path.exists() {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            let mut stderr = String::new();
            child
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("driver exited before metadata was ready: {status}; {stderr}");
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("driver metadata was not ready at {}", path.display());
}

fn write_private_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions).unwrap();
}
