use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn cargo_install_bins_succeeds_on_declared_msrv_and_current_toolchain() {
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .expect("Cargo.toml should be readable");
    assert!(
        manifest
            .lines()
            .any(|line| line == "rust-version = \"1.88\""),
        "Cargo.toml must declare the actual minimum supported Rust version"
    );

    install_and_assert_bins(
        "declared-msrv",
        Command::new("rustup").args(["run", "1.88.0", "cargo"]),
    );
    install_and_assert_bins("current-toolchain", &mut Command::new(env!("CARGO")));
}

fn install_and_assert_bins(name: &str, command: &mut Command) {
    let root = test_root(&format!("cargo-install-layout-{name}"));
    let install_root = root.join("install");
    let target_dir = root.join("target");
    let output = command
        .args(["install", "--path", env!("CARGO_MANIFEST_DIR"), "--root"])
        .arg(&install_root)
        .args(["--locked", "--bins", "--debug", "--offline", "--force"])
        .env("CARGO_TARGET_DIR", &target_dir)
        .output()
        .expect("cargo install probe should run");

    assert!(
        output.status.success(),
        "cargo install failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for binary in ["humanize-plugin-mcp", "humanize-plugin-driver"] {
        let path = install_root.join("bin").join(binary);
        assert!(
            path.is_file(),
            "missing installed binary {}",
            path.display()
        );
    }

    fs::remove_dir_all(&root).expect("install probe root should be removable");
}

fn test_root(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("temp")
        .join(format!("{name}-{}", std::process::id()));
    if root.exists() {
        fs::remove_dir_all(&root).expect("stale install probe root should be removable");
    }
    fs::create_dir_all(&root).expect("install probe root should be creatable");
    root
}
