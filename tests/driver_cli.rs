use std::process::Command;

#[test]
fn driver_version_reports_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_humanize-plugin-driver"))
        .arg("--version")
        .output()
        .expect("driver binary should run");

    assert!(
        output.status.success(),
        "driver --version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("version output should be UTF-8"),
        format!("humanize-plugin-driver {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
