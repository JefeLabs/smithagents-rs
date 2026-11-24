use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::StreamExt;
use smithagents_server::harness::claude::ClaudeCodeHarness;
use smithagents_server::harness::{Harness, HarnessEvent, TurnRequest};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/claude_stream_hello.jsonl"
);

struct Env {
    dir: tempfile::TempDir,
    harness: ClaudeCodeHarness,
}

impl Env {
    fn args_file(&self) -> PathBuf {
        self.dir.path().join("args")
    }
}

/// Builds a per-test wrapper script that exports `vars` and execs fake-claude.
fn setup(vars: &[(&str, &str)]) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/bin/fake-claude");
    let mut script = String::from("#!/bin/sh\n");
    script.push_str(&format!(
        "export FAKE_CLAUDE_ARGS_FILE='{}'\n",
        dir.path().join("args").display()
    ));
    for (k, v) in vars {
        script.push_str(&format!("export {k}='{v}'\n"));
    }
    script.push_str(&format!("exec '{fake}' \"$@\"\n"));
    let wrapper = dir.path().join("claude");
    std::fs::write(&wrapper, script).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    Env {
        dir,
        harness: ClaudeCodeHarness { bin: wrapper },
    }
}

fn req(cwd: &Path, resume: Option<&str>) -> TurnRequest {
    TurnRequest {
        prompt: "hello there".into(),
        cwd: cwd.to_path_buf(),
        resume_session_id: resume.map(str::to_owned),
        model: None,
        system_prompt_append: None,
    }
}

async fn collect(env: &Env, r: TurnRequest) -> Vec<HarnessEvent> {
    env.harness.run_turn(r).await.unwrap().collect().await
}

#[tokio::test]
async fn args_include_stream_json_and_bypass_permissions() {
    let env = setup(&[("FAKE_CLAUDE_FIXTURE", FIXTURE)]);
    let mut r = req(env.dir.path(), None);
    r.model = Some("opus".into());
    r.system_prompt_append = Some("be brief".into());
    collect(&env, r).await;
    let args = std::fs::read_to_string(env.args_file()).unwrap();
    for needle in [
        "-p\nhello there",
        "--output-format\nstream-json",
        "--verbose",
        "--include-partial-messages",
        "--dangerously-skip-permissions",
        "--model\nopus",
        "--append-system-prompt\nbe brief",
    ] {
        assert!(args.contains(needle), "missing {needle:?} in {args:?}");
    }
}

#[tokio::test]
async fn resume_flag_present_only_when_session_given() {
    let env = setup(&[("FAKE_CLAUDE_FIXTURE", FIXTURE)]);
    collect(&env, req(env.dir.path(), Some("abc"))).await;
    let args = std::fs::read_to_string(env.args_file()).unwrap();
    assert!(args.contains("--resume\nabc"), "{args:?}");

    collect(&env, req(env.dir.path(), None)).await;
    let args = std::fs::read_to_string(env.args_file()).unwrap();
    assert!(!args.contains("--resume"), "{args:?}");
    assert!(!args.contains("--model"), "{args:?}");
    assert!(!args.contains("--append-system-prompt"), "{args:?}");
}

#[tokio::test]
async fn streams_events_from_fixture() {
    let env = setup(&[("FAKE_CLAUDE_FIXTURE", FIXTURE)]);
    let events = collect(&env, req(env.dir.path(), None)).await;
    assert!(matches!(
        events.first(),
        Some(HarnessEvent::SessionStarted { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(HarnessEvent::TurnCompleted {
            is_error: false,
            ..
        })
    ));
    assert!(events
        .iter()
        .any(|e| matches!(e, HarnessEvent::AssistantText { .. })));
    assert!(!events
        .iter()
        .any(|e| matches!(e, HarnessEvent::Failed { .. })));
}

#[tokio::test]
async fn nonzero_exit_yields_failed_event() {
    let env = setup(&[
        ("FAKE_CLAUDE_FIXTURE", FIXTURE),
        ("FAKE_CLAUDE_EXIT", "3"),
        ("FAKE_CLAUDE_STDERR", "boom: auth expired"),
    ]);
    let events = collect(&env, req(env.dir.path(), None)).await;
    match events.last() {
        Some(HarnessEvent::Failed { message }) => {
            assert!(message.contains('3'), "{message}");
            assert!(message.contains("boom: auth expired"), "{message}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn zero_exit_without_completion_just_ends() {
    let env = setup(&[]);
    let events = collect(&env, req(env.dir.path(), None)).await;
    assert!(events.is_empty(), "{events:?}");
}

#[tokio::test]
async fn control_dir_overrides_exit_code_per_invocation() {
    let ctl = tempfile::tempdir().unwrap();
    let env = setup(&[
        ("FAKE_CLAUDE_FIXTURE", FIXTURE),
        ("FAKE_CLAUDE_CONTROL_DIR", ctl.path().to_str().unwrap()),
    ]);
    let ok = collect(&env, req(env.dir.path(), None)).await;
    assert!(!ok.iter().any(|e| matches!(e, HarnessEvent::Failed { .. })));
    std::fs::write(ctl.path().join("exit"), "7\n").unwrap();
    let bad = collect(&env, req(env.dir.path(), None)).await;
    assert!(matches!(bad.last(), Some(HarnessEvent::Failed { .. })));
}

#[tokio::test]
async fn malformed_line_is_skipped_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let mixed = dir.path().join("mixed.jsonl");
    let good = std::fs::read_to_string(FIXTURE).unwrap();
    std::fs::write(&mixed, format!("this is not json\n{good}")).unwrap();
    let env = setup(&[("FAKE_CLAUDE_FIXTURE", mixed.to_str().unwrap())]);
    let events = collect(&env, req(env.dir.path(), None)).await;
    assert!(matches!(
        events.last(),
        Some(HarnessEvent::TurnCompleted { .. })
    ));
}

#[tokio::test]
async fn huge_line_is_handled() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.jsonl");
    let blob = "x".repeat(2 * 1024 * 1024);
    let good = std::fs::read_to_string(FIXTURE).unwrap();
    std::fs::write(
        &big,
        format!("{{\"type\":\"unknown\",\"blob\":\"{blob}\"}}\n{good}"),
    )
    .unwrap();
    let env = setup(&[("FAKE_CLAUDE_FIXTURE", big.to_str().unwrap())]);
    let events = collect(&env, req(env.dir.path(), None)).await;
    assert!(matches!(
        events.last(),
        Some(HarnessEvent::TurnCompleted { .. })
    ));
}

fn pid_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn dropping_stream_kills_process() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let env = setup(&[
        ("FAKE_CLAUDE_SLEEP", "30"),
        ("FAKE_CLAUDE_PID_FILE", pid_file.to_str().unwrap()),
    ]);
    let stream = env
        .harness
        .run_turn(req(env.dir.path(), None))
        .await
        .unwrap();
    let start = Instant::now();
    let pid = loop {
        if let Ok(p) = std::fs::read_to_string(&pid_file) {
            if !p.trim().is_empty() {
                break p.trim().to_owned();
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "child never started"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(pid_alive(&pid));
    drop(stream);
    let start = Instant::now();
    while pid_alive(&pid) {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "child still alive"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
