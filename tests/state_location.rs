use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use humanize_plugin::review::ReviewStore;
use humanize_plugin::run_assets::RunAssetStore;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

#[test]
fn explicit_state_overrides_reject_the_current_project_tree_and_symlink_aliases() {
    let root = test_root("overrides");
    let project = root.join("project");
    let outside = root.join("outside");
    fs::create_dir_all(project.join("state")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    symlink(project.join("state"), outside.join("project-state-link")).unwrap();

    for (case, variable, value) in [
        ("state", "HUMANIZE_STATE_ROOT", project.join("state")),
        ("state", "XDG_STATE_HOME", project.join("temp")),
        ("runs", "HUMANIZE_RUNS_DIR", project.join("runs")),
        (
            "state",
            "HUMANIZE_STATE_ROOT",
            outside.join("project-state-link"),
        ),
    ] {
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("state_location_child")
            .arg("--nocapture")
            .current_dir(&project)
            .env("HUMANIZE_STATE_LOCATION_CHILD", case)
            .env_remove("HUMANIZE_STATE_ROOT")
            .env_remove("HUMANIZE_RUNS_DIR")
            .env_remove("XDG_STATE_HOME")
            .env(variable, value)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_state_overrides_use_the_nearest_repository_root_from_nested_working_directories() {
    let root = test_root("nested-repository");

    for marker in ["directory", "file"] {
        let project = root.join(marker);
        let nested = project.join("src");
        let state = project.join("temp/state");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&state).unwrap();
        if marker == "directory" {
            fs::create_dir(project.join(".git")).unwrap();
        } else {
            fs::write(project.join(".git"), "gitdir: ../git-metadata\n").unwrap();
        }

        for (case, variable, value) in [
            ("state", "HUMANIZE_STATE_ROOT", state.clone()),
            ("state", "XDG_STATE_HOME", project.join("temp")),
            ("runs", "HUMANIZE_RUNS_DIR", project.join("temp/runs")),
        ] {
            let output = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("state_location_child")
                .arg("--nocapture")
                .current_dir(&nested)
                .env("HUMANIZE_STATE_LOCATION_CHILD", case)
                .env_remove("HUMANIZE_STATE_ROOT")
                .env_remove("HUMANIZE_RUNS_DIR")
                .env_remove("XDG_STATE_HOME")
                .env(variable, value)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{marker} marker child failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn default_home_state_is_rejected_when_home_is_the_working_directory() {
    let root = test_root("default-home");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("state_location_child")
        .arg("--nocapture")
        .current_dir(&home)
        .env("HUMANIZE_STATE_LOCATION_CHILD", "default")
        .env("HOME", &home)
        .env_remove("HUMANIZE_STATE_ROOT")
        .env_remove("HUMANIZE_RUNS_DIR")
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn default_state_roots_reject_symlink_aliases_into_the_project() {
    let root = test_root("default-symlink-aliases");
    let project = root.join("project");
    let project_state = project.join("private-state");
    let home = root.join("home");
    let xdg = root.join("xdg");
    fs::create_dir_all(&project_state).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&xdg).unwrap();
    symlink(&project_state, home.join(".humanize")).unwrap();
    symlink(&project_state, xdg.join("humanize")).unwrap();

    for (name, home_value, xdg_value) in [
        ("home", Some(home.as_path()), None),
        ("xdg", Some(root.as_path()), Some(xdg.as_path())),
    ] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("state_location_child")
            .arg("--nocapture")
            .current_dir(&project)
            .env("HUMANIZE_STATE_LOCATION_CHILD", "state-default")
            .env_remove("HUMANIZE_STATE_ROOT")
            .env_remove("XDG_STATE_HOME");
        if let Some(home_value) = home_value {
            command.env("HOME", home_value);
        }
        if let Some(xdg_value) = xdg_value {
            command.env("XDG_STATE_HOME", xdg_value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{name} child failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn state_location_child() {
    let Some(case) = std::env::var_os("HUMANIZE_STATE_LOCATION_CHILD") else {
        return;
    };
    match case.to_str().unwrap() {
        "state" => assert!(ReviewStore::runtime_default().is_err()),
        "runs" => assert!(RunAssetStore::runtime_default().runs_root().is_err()),
        "default" => {
            assert!(ReviewStore::runtime_default().is_err());
            assert!(RunAssetStore::runtime_default().runs_root().is_err());
        }
        "state-default" => assert!(ReviewStore::runtime_default().is_err()),
        other => panic!("unknown child case: {other}"),
    }
}

fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "humanize-state-location-{name}-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ))
}
