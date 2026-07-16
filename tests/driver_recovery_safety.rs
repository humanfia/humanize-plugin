use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use humanize_plugin::driver::{
    acquire_driver_attach_lock, cleanup_stale_driver_ipc, socket_path_for_run_root,
};
use serde_json::json;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn attach_lock_uses_canonical_private_root_for_symlinked_public_run_ancestor() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let fixture = StateRootFixture::new("public-run-ancestor");
    let real_runs_root = fixture.root.join("real-runs");
    let linked_runs_root = fixture.root.join("linked-runs");
    let run_root = linked_runs_root.join("run-attach-lock");
    let runtime_root = fixture.state_root.join("runtime");
    fs::create_dir_all(real_runs_root.join("run-attach-lock")).unwrap();
    symlink(&real_runs_root, &linked_runs_root).unwrap();

    let lexical_root = std::path::absolute(&run_root).unwrap();
    let canonical_root = fs::canonicalize(&run_root).unwrap();
    assert_ne!(lexical_root, canonical_root);

    let lock = acquire_driver_attach_lock(&run_root).unwrap();
    let canonical_private_root = runtime_root.join(private_run_root_name(&canonical_root));
    let lexical_private_root = runtime_root.join(private_run_root_name(&lexical_root));
    assert!(canonical_private_root.join("driver/attach.lock").exists());
    assert!(!lexical_private_root.exists());
    drop(lock);

    assert!(
        acquire_driver_attach_lock(&canonical_root).is_ok(),
        "canonical public run path should use the existing private state"
    );
    assert!(!lexical_private_root.exists());
}

#[test]
fn attach_and_cleanup_reuse_legacy_lexical_identity_through_physical_alias() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let fixture = StateRootFixture::new("legacy-public-run-ancestor");
    let real_runs_root = fixture.root.join("real-runs");
    let linked_runs_root = fixture.root.join("linked-runs");
    let real_run_root = real_runs_root.join("run-legacy");
    let linked_run_root = linked_runs_root.join("run-legacy");
    let runtime_root = fixture.state_root.join("runtime");
    fs::create_dir_all(&real_run_root).unwrap();
    fs::create_dir(&runtime_root).unwrap();
    set_mode(&runtime_root, 0o700);
    symlink(&real_runs_root, &linked_runs_root).unwrap();

    let legacy_root = runtime_root.join(private_run_root_name(
        &std::path::absolute(&linked_run_root).unwrap(),
    ));
    fs::create_dir_all(legacy_root.join("driver")).unwrap();
    set_mode(&legacy_root, 0o700);
    set_mode(&legacy_root.join("driver"), 0o700);
    let identity_path = legacy_root.join("identity.json");
    fs::write(
        &identity_path,
        serde_json::to_vec(&json!({
            "schema": "humanize.private_run_identity.v1",
            "run_id": "run-legacy",
            "public_run_root": std::path::absolute(&linked_run_root).unwrap(),
            "runs_root": std::path::absolute(&linked_runs_root).unwrap(),
        }))
        .unwrap(),
    )
    .unwrap();
    set_mode(&identity_path, 0o600);

    let lock = acquire_driver_attach_lock(&real_run_root).unwrap();
    assert!(legacy_root.join("driver/attach.lock").exists());
    drop(lock);
    assert_eq!(
        socket_path_for_run_root(&runtime_root, &linked_run_root).unwrap(),
        legacy_root.join("s")
    );
    assert_eq!(
        socket_path_for_run_root(&runtime_root, &real_run_root).unwrap(),
        legacy_root.join("s")
    );
    cleanup_stale_driver_ipc(&real_run_root, "run-legacy").unwrap();
    assert!(
        !runtime_root
            .join(private_run_root_name(
                &fs::canonicalize(&real_run_root).unwrap()
            ))
            .exists()
    );
}

struct StateRootFixture {
    root: PathBuf,
    state_root: PathBuf,
    prior_state_root: Option<OsString>,
}

impl StateRootFixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .join("humanize-plugin-driver-recovery-safety")
            .join(format!("{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        set_mode(&root, 0o700);
        let state_root = root.join("state");
        fs::create_dir(&state_root).unwrap();
        set_mode(&state_root, 0o700);
        let prior_state_root = std::env::var_os("HUMANIZE_STATE_ROOT");
        unsafe {
            std::env::set_var("HUMANIZE_STATE_ROOT", &state_root);
        }
        Self {
            root,
            state_root,
            prior_state_root,
        }
    }
}

impl Drop for StateRootFixture {
    fn drop(&mut self) {
        unsafe {
            match self.prior_state_root.take() {
                Some(value) => std::env::set_var("HUMANIZE_STATE_ROOT", value),
                None => std::env::remove_var("HUMANIZE_STATE_ROOT"),
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn private_run_root_name(public_run_root: &Path) -> String {
    format!("r{:016x}", stable_hash(&public_run_root.to_string_lossy()))
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
