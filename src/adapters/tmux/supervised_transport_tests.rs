use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::*;
use crate::input_ledger::{MachineInputLedger, MachineInputStatus};

#[derive(Clone)]
struct IsolatedTmuxRunner {
    socket_name: String,
}

impl IsolatedTmuxRunner {
    fn run_tmux(&self, arguments: &[&str]) -> CommandOutput {
        let output = Command::new("tmux")
            .arg("-L")
            .arg(&self.socket_name)
            .args(arguments)
            .output()
            .expect("real tmux must be available for supervised transport tests");
        CommandOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

impl CommandRunner for IsolatedTmuxRunner {
    fn run(&self, argv: Vec<String>) -> Result<CommandOutput, TmuxError> {
        let Some((program, arguments)) = argv.split_first() else {
            return Err(TmuxError::EmptyArgv);
        };
        assert_eq!(program, "tmux");
        let output = Command::new("tmux")
            .arg("-L")
            .arg(&self.socket_name)
            .args(arguments)
            .output()
            .map_err(|error| TmuxError::io(&argv, &error.to_string()))?;
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

struct RealTmuxFixture {
    runner: IsolatedTmuxRunner,
    root: PathBuf,
}

impl RealTmuxFixture {
    fn delayed_codex(delay: Duration) -> (Self, TmuxActivationMetadata, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("temp")
            .join(format!("tmux-delayed-codex-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let prompt_path = root.join("prompt.txt");
        let script = format!(
            "sleep {}; printf '\\033[2J\\033[HOpenAI Codex (v0.144.4)\\nmodel: test-model\\ndirectory: /tmp\\npermissions: test\\n\\n'; IFS= read -r prompt; printf '%s' \"$prompt\" > '{}' ; sleep 5",
            delay.as_secs_f64(),
            prompt_path.display()
        );
        let runner = IsolatedTmuxRunner {
            socket_name: format!("humanize-supervised-{}-{nonce}", std::process::id()),
        };
        let output = runner.run_tmux(&[
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{window_id}|#{pane_id}",
            "-s",
            "humanize-supervised",
            "-n",
            "agent",
            "sh",
            "-c",
            &script,
        ]);
        assert!(output.is_success(), "{}", output.stderr);
        let (window_id, pane_id) = output.stdout.trim().split_once('|').unwrap();
        let metadata = TmuxActivationMetadata::new(
            "humanize-supervised",
            "run-delayed-codex",
            "agent",
            window_id,
            "root",
            pane_id,
        );
        (Self { runner, root }, metadata, prompt_path)
    }

    fn agent_transport(
        profile: &str,
        delay: Duration,
        expected_enters: usize,
        accept_submission: bool,
    ) -> (Self, TmuxActivationMetadata, PathBuf, PathBuf, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("temp")
            .join(format!(
                "tmux-accepting-codex-{}-{nonce}",
                std::process::id()
            ));
        fs::create_dir_all(&root).unwrap();
        let script_path = root.join("fake_tui.py");
        let prompt_path = root.join("prompt.bin");
        let enter_count_path = root.join("enter-count.txt");
        let early_input_path = root.join("early-input.bin");
        fs::write(&script_path, FAKE_CODEX_TUI).unwrap();
        let runner = IsolatedTmuxRunner {
            socket_name: format!("humanize-supervised-{}-{nonce}", std::process::id()),
        };
        let output = runner.run_tmux(&[
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{window_id}|#{pane_id}",
            "-s",
            "humanize-supervised",
            "-n",
            "agent",
            "python3",
            script_path.to_str().unwrap(),
            &delay.as_secs_f64().to_string(),
            prompt_path.to_str().unwrap(),
            enter_count_path.to_str().unwrap(),
            early_input_path.to_str().unwrap(),
            &expected_enters.to_string(),
            profile,
            if accept_submission {
                "accept"
            } else {
                "ignore"
            },
        ]);
        assert!(output.is_success(), "{}", output.stderr);
        let (window_id, pane_id) = output.stdout.trim().split_once('|').unwrap();
        let metadata = TmuxActivationMetadata::new(
            "humanize-supervised",
            "run-accepting-codex",
            "agent",
            window_id,
            "root",
            pane_id,
        );
        (
            Self { runner, root },
            metadata,
            prompt_path,
            enter_count_path,
            early_input_path,
        )
    }
}

impl Drop for RealTmuxFixture {
    fn drop(&mut self) {
        let _ = self.runner.run_tmux(&["kill-server"]);
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn inferred_codex_readiness_waits_for_delayed_input_surface_before_delivery() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let delay = Duration::from_millis(400);
    let (fixture, metadata, prompt_path) = RealTmuxFixture::delayed_codex(delay);
    let adapter = TmuxAdapter::with_runner(fixture.runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(MachineInputLedger::in_memory(), 1_000),
    );

    let started = Instant::now();
    adapter
        .wait_for_inferred_agent_readiness(
            &metadata,
            "codex --dangerously-bypass-approvals-and-sandbox",
            Duration::from_secs(2),
        )
        .unwrap();
    assert!(started.elapsed() >= delay);
    assert!(!prompt_path.exists());

    adapter
        .send_clean_input_transaction(&metadata, "delayed Codex prompt")
        .unwrap();
    wait_for(Duration::from_secs(2), || prompt_path.exists());
    assert_eq!(
        fs::read_to_string(prompt_path).unwrap(),
        "delayed Codex prompt"
    );
}

#[test]
fn known_codex_submission_preserves_prompt_and_requires_acceptance() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let (fixture, metadata, prompt_path, enter_count_path, early_input_path) =
        RealTmuxFixture::agent_transport("codex", Duration::from_millis(300), 2, true);
    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(fixture.runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(ledger.clone(), 1_000),
    );
    let prompt = "first line\nsecond '$HOME' \\ path\nthird line";

    adapter
        .wait_for_inferred_agent_readiness(&metadata, "codex", Duration::from_secs(2))
        .unwrap();
    let transaction = adapter
        .send_clean_input_transaction_with_agent_acceptance(
            &metadata,
            prompt,
            2,
            "codex",
            Duration::from_secs(2),
        )
        .unwrap();

    assert_eq!(transaction.record().status, MachineInputStatus::Submitted);
    assert_eq!(transaction.acceptance().unwrap().profile(), "codex");
    assert_eq!(transaction.acceptance().unwrap().signal(), "working_state");
    assert_eq!(fs::read(&prompt_path).unwrap(), prompt.as_bytes());
    assert_eq!(fs::read_to_string(enter_count_path).unwrap(), "2");
    assert!(!early_input_path.exists());
}

#[test]
fn known_codex_submission_without_marker_is_submitted_without_acceptance() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let (fixture, metadata, prompt_path, enter_count_path, early_input_path) =
        RealTmuxFixture::agent_transport("codex", Duration::from_millis(100), 1, false);
    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(fixture.runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(ledger.clone(), 1_000),
    );

    adapter
        .wait_for_inferred_agent_readiness(&metadata, "codex", Duration::from_secs(2))
        .unwrap();
    let transaction = adapter
        .send_clean_input_transaction_with_agent_acceptance(
            &metadata,
            "acceptance must be observed",
            1,
            "codex",
            Duration::from_millis(250),
        )
        .unwrap();

    assert_eq!(transaction.record().status, MachineInputStatus::Submitted);
    assert!(transaction.acceptance().is_none());
    wait_for(Duration::from_secs(1), || {
        prompt_path.exists()
            && fs::read_to_string(&enter_count_path).is_ok_and(|count| count == "1")
    });
    assert_eq!(
        fs::read(&prompt_path).unwrap(),
        b"acceptance must be observed"
    );
    assert_eq!(fs::read_to_string(enter_count_path).unwrap(), "1");
    assert!(!early_input_path.exists());
    assert_eq!(
        ledger
            .records()
            .into_iter()
            .map(|record| record.status)
            .collect::<Vec<_>>(),
        vec![MachineInputStatus::Started, MachineInputStatus::Submitted]
    );
}

#[test]
fn known_claude_transport_waits_and_requires_acceptance() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let delay = Duration::from_millis(250);
    let (fixture, metadata, prompt_path, enter_count_path, early_input_path) =
        RealTmuxFixture::agent_transport("claude", delay, 1, true);
    let adapter = TmuxAdapter::with_runner(fixture.runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(MachineInputLedger::in_memory(), 1_000),
    );
    let started = Instant::now();

    adapter
        .wait_for_inferred_agent_readiness(
            &metadata,
            "claude --dangerously-skip-permissions",
            Duration::from_secs(2),
        )
        .unwrap();
    assert!(started.elapsed() >= delay);
    adapter
        .send_clean_input_transaction_with_agent_acceptance(
            &metadata,
            "review the transport",
            1,
            "claude --dangerously-skip-permissions",
            Duration::from_secs(2),
        )
        .unwrap();

    assert_eq!(fs::read(&prompt_path).unwrap(), b"review the transport");
    assert_eq!(fs::read_to_string(enter_count_path).unwrap(), "1");
    assert!(!early_input_path.exists());
}

#[test]
fn known_omp_transport_waits_and_requires_acceptance() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let delay = Duration::from_millis(200);
    let (fixture, metadata, prompt_path, enter_count_path, early_input_path) =
        RealTmuxFixture::agent_transport("omp", delay, 1, true);
    let adapter = TmuxAdapter::with_runner(fixture.runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(MachineInputLedger::in_memory(), 1_000),
    );
    let started = Instant::now();

    adapter
        .wait_for_inferred_agent_readiness(
            &metadata,
            "/home/agent/.bun/bin/omp",
            Duration::from_secs(2),
        )
        .unwrap();
    assert!(started.elapsed() >= delay);
    adapter
        .send_clean_input_transaction_with_agent_acceptance(
            &metadata,
            "inspect OMP transport",
            1,
            "/home/agent/.bun/bin/omp",
            Duration::from_secs(2),
        )
        .unwrap();

    assert_eq!(fs::read(&prompt_path).unwrap(), b"inspect OMP transport");
    assert_eq!(fs::read_to_string(enter_count_path).unwrap(), "1");
    assert!(!early_input_path.exists());
}

#[test]
fn explicit_ready_pattern_overrides_inferred_codex_surface() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let delay = Duration::from_millis(200);
    let (fixture, metadata, prompt_path, enter_count_path, _) =
        RealTmuxFixture::agent_transport("custom", delay, 1, true);
    let adapter = TmuxAdapter::with_runner(fixture.runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(MachineInputLedger::in_memory(), 1_000),
    );
    let started = Instant::now();

    adapter
        .wait_for_pane_text(&metadata, "CUSTOM INPUT READY", Duration::from_secs(2))
        .unwrap();
    assert!(started.elapsed() >= delay);
    adapter
        .send_clean_input_transaction_with_agent_acceptance(
            &metadata,
            "explicit pattern prompt",
            1,
            "codex",
            Duration::from_secs(2),
        )
        .unwrap();

    assert_eq!(fs::read(&prompt_path).unwrap(), b"explicit pattern prompt");
    assert_eq!(fs::read_to_string(enter_count_path).unwrap(), "1");
}

#[test]
fn unknown_harness_remains_usable_without_acceptance_profile() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let (fixture, metadata, prompt_path, enter_count_path, _) =
        RealTmuxFixture::agent_transport("custom", Duration::ZERO, 1, false);
    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(fixture.runner.clone())
        .with_input_transaction_config(TmuxInputTransactionConfig::deterministic(ledger, 1_000));

    assert_eq!(
        adapter
            .wait_for_inferred_agent_readiness(
                &metadata,
                "custom-agent --interactive",
                Duration::from_millis(50),
            )
            .unwrap(),
        None
    );
    adapter
        .wait_for_pane_text(&metadata, "CUSTOM INPUT READY", Duration::from_secs(2))
        .unwrap();
    let transaction = adapter
        .send_clean_input_transaction_with_agent_acceptance(
            &metadata,
            "generic prompt",
            1,
            "custom-agent --interactive",
            Duration::from_millis(50),
        )
        .unwrap();

    wait_for(Duration::from_secs(1), || {
        prompt_path.exists()
            && fs::read_to_string(&enter_count_path).is_ok_and(|count| count == "1")
    });
    assert!(transaction.acceptance().is_none());
    assert_eq!(transaction.record().status, MachineInputStatus::Submitted);
    assert_eq!(fs::read(&prompt_path).unwrap(), b"generic prompt");
    assert_eq!(fs::read_to_string(enter_count_path).unwrap(), "1");
}

#[test]
fn inferred_readiness_timeout_sends_no_input() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let (fixture, metadata, prompt_path, enter_count_path, early_input_path) =
        RealTmuxFixture::agent_transport("codex", Duration::from_millis(500), 1, true);
    let adapter = TmuxAdapter::with_runner(fixture.runner.clone());

    let error = adapter
        .wait_for_inferred_agent_readiness(&metadata, "codex", Duration::from_millis(100))
        .unwrap_err();

    assert!(matches!(error, TmuxError::AgentReadinessTimeout { .. }));
    assert!(!prompt_path.exists());
    assert!(!enter_count_path.exists());
    assert!(!early_input_path.exists());
}

#[test]
fn pane_replacement_after_paste_prevents_enter_and_submission() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let (fixture, metadata, prompt_path, enter_count_path, _) =
        RealTmuxFixture::agent_transport("codex", Duration::ZERO, 1, true);
    let ledger = MachineInputLedger::in_memory();
    let adapter = TmuxAdapter::with_runner(fixture.runner.clone()).with_input_transaction_config(
        TmuxInputTransactionConfig::deterministic(ledger.clone(), 1_000)
            .with_prompt_to_submit_delay(Duration::from_millis(300)),
    );
    adapter
        .wait_for_inferred_agent_readiness(&metadata, "codex", Duration::from_secs(2))
        .unwrap();
    let runner = fixture.runner.clone();
    let prompt_for_replacement = prompt_path.clone();
    let target = format!("{}:{}", metadata.session_id(), metadata.window_id());
    let replacer = thread::spawn(move || {
        wait_for(Duration::from_secs(1), || prompt_for_replacement.exists());
        let output = runner.run_tmux(&["rename-window", "-t", &target, "replacement"]);
        assert!(output.is_success(), "{}", output.stderr);
    });

    let error = adapter
        .send_clean_input_transaction_with_agent_acceptance(
            &metadata,
            "do not submit to a replacement",
            1,
            "codex",
            Duration::from_secs(1),
        )
        .unwrap_err();
    replacer.join().unwrap();

    assert!(matches!(error, TmuxError::PaneMetadataMismatch(_)));
    assert_eq!(
        fs::read(&prompt_path).unwrap(),
        b"do not submit to a replacement"
    );
    assert!(!enter_count_path.exists());
    assert_eq!(
        ledger
            .records()
            .into_iter()
            .map(|record| record.status)
            .collect::<Vec<_>>(),
        vec![MachineInputStatus::Started, MachineInputStatus::Failed]
    );
}

fn wait_for(timeout: Duration, predicate: impl Fn() -> bool) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("condition was not satisfied within {timeout:?}");
}

const FAKE_CODEX_TUI: &str = r#"import os
import select
import sys
import termios
import time
import tty

delay = float(sys.argv[1])
prompt_path = sys.argv[2]
enter_count_path = sys.argv[3]
early_input_path = sys.argv[4]
expected_enters = int(sys.argv[5])
profile = sys.argv[6]
accept_submission = sys.argv[7] == "accept"
old_attributes = termios.tcgetattr(0)
tty.setraw(0)
try:
    deadline = time.monotonic() + delay
    early_input = bytearray()
    while time.monotonic() < deadline:
        remaining = max(0.0, deadline - time.monotonic())
        readable, _, _ = select.select([0], [], [], min(0.02, remaining))
        if readable:
            early_input.extend(os.read(0, 4096))
    if early_input:
        with open(early_input_path, "wb") as output:
            output.write(early_input)

    ready_surfaces = {
        "codex": b"OpenAI Codex (v0.144.4)\nmodel: test-model\ndirectory: /tmp\npermissions: test\n\n",
        "claude": b"Claude Code v2.1.210\nWelcome back!\nbypass permissions on\n\n",
        "omp": b"omp v16.3.3\nWelcome back!\n# for prompt actions\n/ for commands\n\n",
        "custom": b"CUSTOM INPUT READY\n\n",
    }
    os.write(1, b"\x1b[?2004h")
    os.write(1, b"\x1b[2J\x1b[H" + ready_surfaces[profile])
    stream = bytearray()
    prompt = None
    enter_count = 0
    while enter_count < expected_enters:
        stream.extend(os.read(0, 4096))
        if prompt is None:
            start = stream.find(b"\x1b[200~")
            end = stream.find(b"\x1b[201~", start + 6) if start >= 0 else -1
            if start >= 0 and end >= 0:
                prompt = bytes(stream[start + 6:end])
                with open(prompt_path, "wb") as output:
                    output.write(prompt)
                stream = stream[end + 6:]
        if prompt is not None:
            enter_count += stream.count(b"\r")
            stream.clear()

    with open(enter_count_path, "w", encoding="ascii") as output:
        output.write(str(enter_count))
    if accept_submission:
        os.write(1, b"\x1b[2J\x1b[HWorking (0s - esc to interrupt)\n")
    time.sleep(5)
finally:
    termios.tcsetattr(0, termios.TCSADRAIN, old_attributes)
"#;
