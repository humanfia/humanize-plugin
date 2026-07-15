use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::flow::{
    FlowCheckMode, FlowDraft, FlowLock, FlowNode, FlowResource, ResourceKind, flow_lock,
};
use sha2::{Digest, Sha256};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

fn package_root(name: &str) -> PathBuf {
    let index = NEXT_ROOT.fetch_add(1, Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("flow-package-{name}-{index}"));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    root
}

fn draft(readme: &str) -> FlowDraft {
    FlowDraft {
        nodes: vec![FlowNode {
            id: "root".into(),
            ..FlowNode::default()
        }],
        resources: vec![
            FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: readme.into(),
            },
            FlowResource {
                id: "skills/audit/SKILL.md".into(),
                kind: ResourceKind::Skill,
                source: "# Audit\n\nInspect the repository.\n".into(),
            },
        ],
        ..FlowDraft::default()
    }
}

#[test]
fn flow_lock_uses_one_sha256_identity_and_direct_structured_serde() {
    let lock = flow_lock(&draft("# Package\n\nVerbatim.\n"), FlowCheckMode::Core).unwrap();
    let digest = lock
        .content_hash()
        .strip_prefix("sha256:")
        .expect("content hash should use SHA-256");

    assert_eq!(digest.len(), 64);
    assert_eq!(lock.id(), format!("flk_{digest}"));
    assert_eq!(
        lock.content_hash(),
        format!("sha256:{:x}", Sha256::digest(lock.canonical_bytes()))
    );

    let value = serde_json::to_value(&lock).unwrap();
    assert!(value["flow"].is_object());
    assert!(value.get("content").is_none());
    assert!(value.get("normalized_content").is_none());
    assert_eq!(value["content_hash"], lock.content_hash());

    let restored: FlowLock = serde_json::from_value(value).unwrap();
    assert_eq!(restored, lock);
}

#[test]
fn directory_package_preserves_readme_and_skill_and_detects_tampering() {
    let root = package_root("round-trip");
    let readme = "# Package\n\nKeep  two spaces.\n";
    let lock = flow_lock(&draft(readme), FlowCheckMode::Core).unwrap();

    lock.write_directory(&root).unwrap();
    assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), readme);
    assert_eq!(
        fs::read_to_string(root.join("skills/audit/SKILL.md")).unwrap(),
        "# Audit\n\nInspect the repository.\n"
    );
    assert_eq!(FlowLock::load_directory(&root).unwrap(), lock);

    fs::write(root.join("README.md"), "tampered\n").unwrap();
    assert!(FlowLock::load_directory(&root).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_files_use_private_modes() {
    use std::os::unix::fs::PermissionsExt;

    let root = package_root("modes");
    let lock = flow_lock(&draft("README\n"), FlowCheckMode::Core).unwrap();

    lock.write_directory(&root).unwrap();

    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for path in ["flow.json", "README.md", "skills/audit/SKILL.md"] {
        assert_eq!(
            fs::metadata(root.join(path)).unwrap().permissions().mode() & 0o777,
            0o600,
            "{path} should be private"
        );
    }
    assert_eq!(
        fs::metadata(root.join("skills"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(root.join("skills/audit"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_load_rejects_wrong_directory_and_file_modes() {
    use std::os::unix::fs::PermissionsExt;

    for (name, relative, mode) in [
        ("root-mode", "", 0o755),
        ("subdir-mode", "skills", 0o755),
        ("file-mode", "README.md", 0o644),
    ] {
        let root = package_root(name);
        let lock = flow_lock(&draft("README\n"), FlowCheckMode::Core).unwrap();
        lock.write_directory(&root).unwrap();
        let path = if relative.is_empty() {
            root.clone()
        } else {
            root.join(relative)
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert!(FlowLock::load_directory(&root).is_err(), "{name}");
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn package_write_cleans_safe_stale_staging_directories() {
    use std::os::unix::fs::PermissionsExt;

    let root = package_root("staging-retry");
    let stale = root.parent().unwrap().join(format!(
        ".{}.staging-stale",
        root.file_name().unwrap().to_string_lossy()
    ));
    if stale.exists() {
        fs::remove_dir_all(&stale).unwrap();
    }
    fs::create_dir(&stale).unwrap();
    fs::set_permissions(&stale, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(stale.join("partial"), "partial").unwrap();

    let lock = flow_lock(&draft("README\n"), FlowCheckMode::Core).unwrap();
    lock.write_directory(&root).unwrap();

    assert_eq!(FlowLock::load_directory(&root).unwrap(), lock);
    assert!(!stale.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_load_rejects_partial_directories_and_retry_preserves_the_first_package() {
    use std::os::unix::fs::PermissionsExt;

    let partial = package_root("partial");
    fs::create_dir(&partial).unwrap();
    fs::set_permissions(&partial, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(partial.join("flow.json"), "{}\n").unwrap();
    fs::set_permissions(partial.join("flow.json"), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(FlowLock::load_directory(&partial).is_err());
    fs::remove_dir_all(partial).unwrap();

    let root = package_root("retry");
    let first = flow_lock(&draft("First README\n"), FlowCheckMode::Core).unwrap();
    let second = flow_lock(&draft("Second README\n"), FlowCheckMode::Core).unwrap();
    first.write_directory(&root).unwrap();
    assert!(second.write_directory(&root).is_err());
    assert_eq!(FlowLock::load_directory(&root).unwrap(), first);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_fresh_test_process_can_load_the_directory_package() {
    let root = package_root("cross-process");
    let lock = flow_lock(&draft("# Package\n\nCross process.\n"), FlowCheckMode::Core).unwrap();
    lock.write_directory(&root).unwrap();

    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("flow_package_child_loads_directory")
        .arg("--nocapture")
        .env("HUMANIZE_FLOW_PACKAGE_CHILD", &root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "child failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(lock.content_hash()));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn flow_package_child_loads_directory() {
    let Some(root) = std::env::var_os("HUMANIZE_FLOW_PACKAGE_CHILD") else {
        return;
    };
    let lock = FlowLock::load_directory(&PathBuf::from(root)).unwrap();
    println!("{}", lock.content_hash());
}

#[test]
fn lock_rejects_absolute_parent_and_flow_manifest_resource_paths() {
    for path in [
        "/tmp/README.md",
        "../README.md",
        "nested/../README.md",
        "flow.json",
    ] {
        let mut invalid = draft("README\n");
        invalid.resources[0].id = path.into();
        let error = flow_lock(&invalid, FlowCheckMode::Core).unwrap_err();
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "FLOW_INVALID_RESOURCE_PATH"),
            "path {path} should be rejected"
        );
    }
}

#[test]
fn lock_requires_exactly_one_root_readme_and_canonical_skill_paths() {
    let cases = [
        ("docs/README.md", ResourceKind::Readme),
        ("README.txt", ResourceKind::Readme),
        ("skills/audit.md", ResourceKind::Skill),
        ("skills/audit/guide.md", ResourceKind::Skill),
        ("skills/audit/nested/SKILL.md", ResourceKind::Skill),
    ];

    for (path, kind) in cases {
        let mut invalid = draft("README\n");
        invalid.resources[1] = FlowResource {
            id: path.into(),
            kind,
            source: "content\n".into(),
        };
        let error = flow_lock(&invalid, FlowCheckMode::Core).unwrap_err();
        assert!(
            error.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic.code.as_str(),
                "FLOW_INVALID_README_PATH" | "FLOW_INVALID_SKILL_PATH"
            )),
            "path {path} should be rejected: {:?}",
            error.diagnostics
        );
    }

    let mut duplicate = draft("README\n");
    duplicate.resources.push(FlowResource {
        id: "README.md".into(),
        kind: ResourceKind::Readme,
        source: "second\n".into(),
    });
    let error = flow_lock(&duplicate, FlowCheckMode::Core).unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "FLOW_DUPLICATE_RESOURCE_PATH")
    );
}

#[test]
fn lock_rejects_unresolved_embedded_resource_references() {
    let mut invalid = draft("README\n");
    invalid.imports.push(humanize_plugin::flow::FlowImport {
        resource_id: "prompts/missing.md".into(),
        alias: Some("missing".into()),
    });

    let error = flow_lock(&invalid, FlowCheckMode::Core).unwrap_err();

    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "FLOW_UNRESOLVED_IMPORT")
    );
}

#[cfg(unix)]
#[test]
fn directory_package_rejects_symlink_and_hardlink_resources() {
    use std::os::unix::fs::symlink;

    let symlink_root = package_root("symlink");
    let lock = flow_lock(&draft("README\n"), FlowCheckMode::Core).unwrap();
    lock.write_directory(&symlink_root).unwrap();
    fs::remove_file(symlink_root.join("README.md")).unwrap();
    symlink("skills/audit/SKILL.md", symlink_root.join("README.md")).unwrap();
    assert!(FlowLock::load_directory(&symlink_root).is_err());
    fs::remove_dir_all(symlink_root).unwrap();

    let hardlink_root = package_root("hardlink");
    lock.write_directory(&hardlink_root).unwrap();
    let outside = hardlink_root.with_extension("outside");
    fs::write(&outside, "README\n").unwrap();
    fs::remove_file(hardlink_root.join("README.md")).unwrap();
    fs::hard_link(&outside, hardlink_root.join("README.md")).unwrap();
    assert!(FlowLock::load_directory(&hardlink_root).is_err());
    fs::remove_dir_all(hardlink_root).unwrap();
    fs::remove_file(outside).unwrap();

    let fifo_root = package_root("fifo");
    lock.write_directory(&fifo_root).unwrap();
    let readme = fifo_root.join("README.md");
    fs::remove_file(&readme).unwrap();
    let path = std::ffi::CString::new(readme.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: path is a valid NUL-terminated path and mkfifo does not retain it.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(FlowLock::load_directory(&fifo_root).is_err());
    fs::remove_dir_all(fifo_root).unwrap();
}
