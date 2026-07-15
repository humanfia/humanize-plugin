use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::flow::{
    FlowCheckMode, FlowDraft, FlowLock, FlowNode, FlowResource, ResourceKind, flow_lock,
};
use humanize_plugin::review::{ReviewDecision, ReviewStatus, ReviewStore};
use serde_json::json;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

fn review_root(name: &str) -> PathBuf {
    let index = NEXT_ROOT.fetch_add(1, Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("flow-review-{name}-{index}"));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    root
}

fn review_lock(name: &str) -> FlowLock {
    flow_lock(
        &FlowDraft {
            nodes: vec![FlowNode {
                id: "root".into(),
                ..FlowNode::default()
            }],
            resources: vec![FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: format!("Review fixture {name}.\n"),
            }],
            ..FlowDraft::default()
        },
        FlowCheckMode::Core,
    )
    .unwrap()
}

#[test]
fn prepared_review_persists_with_private_deterministic_files_and_uri() {
    let root = review_root("persist with % encoding");
    let store = ReviewStore::new(root.clone());
    let lock = review_lock("persist");
    let snapshot = json!({"title":"Review A","nodes":["root"]});
    let html = "<!doctype html>\n<title>Review A</title>\n";

    let first = store.prepare(&lock, &snapshot, html).unwrap();
    let second = ReviewStore::new(root.clone())
        .load(first.review_id())
        .unwrap();

    assert_eq!(first, second);
    assert_eq!(first.status(), ReviewStatus::Pending);
    assert_eq!(first.document_uri(), second.document_uri());
    assert!(first.document_uri().starts_with("file://"));
    assert!(!first.document_uri().contains(' '));
    assert!(first.document_uri().contains("%20"));
    assert!(first.document_uri().contains("%25"));
    let projection = fs::read_to_string(first.review_json_path()).unwrap();
    assert!(projection.contains("\"derived_from\""));
    assert!(projection.contains(lock.content_hash()));
    assert!(projection.contains("\"projection\""));
    let document = fs::read_to_string(first.document_path()).unwrap();
    assert!(document.contains(&format!("derived_from: {}", first.review_id())));
    assert!(document.ends_with(html));
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for path in [
        root.join("review-mac.key"),
        first.review_directory().join("prepared.json"),
        first.review_json_path().to_path_buf(),
        first.document_path().to_path_buf(),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(fs::read(root.join("review-mac.key")).unwrap().len(), 32);
    assert_eq!(
        fs::metadata(first.review_directory())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn terminal_decisions_are_immutable_and_rejection_or_bypass_requires_reason() {
    let root = review_root("decisions");
    let store = ReviewStore::new(root.clone());
    let lock = review_lock("decisions");
    let review = store
        .prepare(
            &lock,
            &json!({"title":"Review B"}),
            "<title>Review B</title>\n",
        )
        .unwrap();

    for decision in [ReviewDecision::Rejected, ReviewDecision::Bypassed] {
        assert!(store.decide(review.review_id(), decision, None).is_err());
    }

    let decided = store
        .decide(
            review.review_id(),
            ReviewDecision::Bypassed,
            Some("operator accepted the risk"),
        )
        .unwrap();
    assert_eq!(decided.status(), ReviewStatus::Bypassed);
    assert_eq!(decided.reason(), Some("operator accepted the risk"));
    assert!(
        store
            .decide(review.review_id(), ReviewDecision::Approved, None)
            .is_err()
    );

    let reloaded = ReviewStore::new(root.clone())
        .load(review.review_id())
        .unwrap();
    assert_eq!(reloaded.status(), ReviewStatus::Bypassed);
    assert_eq!(
        fs::metadata(reloaded.decision_path().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn review_store_rejects_symlinked_authority_entries() {
    let root = review_root("symlink");
    let store = ReviewStore::new(root.clone());
    let lock = review_lock("symlink");
    let review = store
        .prepare(
            &lock,
            &json!({"title":"Review C"}),
            "<title>Review C</title>\n",
        )
        .unwrap();
    let outside = root.with_extension("outside");
    fs::write(&outside, "forged\n").unwrap();
    let prepared = review.review_directory().join("prepared.json");
    fs::remove_file(&prepared).unwrap();
    symlink(&outside, &prepared).unwrap();

    assert!(store.load(review.review_id()).is_err());

    fs::remove_dir_all(root).unwrap();
    fs::remove_file(outside).unwrap();
}

#[test]
fn review_projection_is_replaceable_cache_but_authority_and_decision_are_mac_protected() {
    let root = review_root("authority-mac");
    let store = ReviewStore::new(root.clone());
    let lock = review_lock("signed");
    let review = store
        .prepare(
            &lock,
            &json!({"title":"Signed review"}),
            "<title>Signed review</title>\n",
        )
        .unwrap();

    fs::write(
        review.review_json_path(),
        "{\"derived_from\":\"forged\",\"projection\":{}}\n",
    )
    .unwrap();
    assert!(store.load(review.review_id()).is_ok());

    let refreshed = store
        .prepare(
            &lock,
            &json!({"title":"Regenerated review"}),
            "<title>Regenerated review</title>\n",
        )
        .unwrap();
    let projection = fs::read_to_string(refreshed.review_json_path()).unwrap();
    assert!(projection.contains("Regenerated review"));
    assert!(projection.contains(lock.content_hash()));

    let rejected = store
        .decide(
            review.review_id(),
            ReviewDecision::Rejected,
            Some("not approved"),
        )
        .unwrap();
    let decision_path = rejected.decision_path().unwrap();
    let forged = fs::read_to_string(&decision_path)
        .unwrap()
        .replace("rejected", "approved");
    fs::write(&decision_path, forged).unwrap();
    assert!(store.load(review.review_id()).is_err());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn review_prepared_authority_rejects_bitflips() {
    let root = review_root("authority-bitflip");
    let store = ReviewStore::new(root.clone());
    let lock = review_lock("bitflip");
    let review = store
        .prepare(
            &lock,
            &json!({"title":"Bitflip review"}),
            "<title>Bitflip review</title>\n",
        )
        .unwrap();
    let prepared_path = review.review_directory().join("prepared.json");
    let mut bytes = fs::read(&prepared_path).unwrap();
    let index = bytes.iter().position(|byte| *byte == b'f').unwrap();
    bytes[index] = b'g';
    fs::write(&prepared_path, bytes).unwrap();

    assert!(store.load(review.review_id()).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn review_authority_rejects_wrong_modes_hardlinks_and_fifos() {
    for case in ["root-mode", "prepared-mode", "hardlink", "fifo"] {
        let root = review_root(case);
        let store = ReviewStore::new(root.clone());
        let lock = review_lock(case);
        let review = store
            .prepare(
                &lock,
                &json!({"title":"Secure review"}),
                "<title>Secure review</title>\n",
            )
            .unwrap();
        let prepared = review.review_directory().join("prepared.json");
        match case {
            "root-mode" => {
                fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
            }
            "prepared-mode" => {
                fs::set_permissions(&prepared, fs::Permissions::from_mode(0o644)).unwrap();
            }
            "hardlink" => {
                let outside = root.with_extension("outside");
                fs::hard_link(&prepared, &outside).unwrap();
            }
            "fifo" => {
                fs::remove_file(&prepared).unwrap();
                let path = std::ffi::CString::new(prepared.as_os_str().as_encoded_bytes()).unwrap();
                // SAFETY: path is a valid NUL-terminated path and mkfifo does not retain it.
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
            _ => unreachable!(),
        }
        assert!(store.load(review.review_id()).is_err(), "{case}");
        if case == "root-mode" {
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        if case == "hardlink" {
            fs::remove_file(root.with_extension("outside")).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }
}
