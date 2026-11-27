use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::StreamExt;
use smithagents_server::harness::claude::ClaudeCodeHarness;
use smithagents_server::harness::{
    Harness, HarnessError, HarnessEvent, StructuredRequest, TurnRequest,
};

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
    // `-p` is a flag; the prompt goes to stdin, never on the command line.
    assert!(args.lines().any(|arg| arg == "-p"), "{args:?}");
    assert!(!args.contains("hello there"), "{args:?}");
    for needle in [
        "--output-format\nstream-json",
        "--verbose",
        "--include-partial-messages",
        "--dangerously-skip-permissions",
        "--model\nopus",
        "--append-system-prompt\nbe brief",
    ] {
        assert!(args.contains(needle), "missing {needle:?} in {args:?}");
    }
    // Chat turns keep the user's MCP servers; only polish runs drop them.
    assert!(
        !args.lines().any(|arg| arg.contains("mcp-config")),
        "{args:?}"
    );
}

/// Runs one turn with `prompt` against a fake-claude that records its stdin,
/// and returns the events, what arrived on stdin and the argv.
async fn turn_with_prompt(prompt: &str) -> (Vec<HarnessEvent>, Vec<u8>, String) {
    let scratch = tempfile::tempdir().unwrap();
    let stdin_file = scratch.path().join("stdin");
    let env = setup(&[
        ("FAKE_CLAUDE_FIXTURE", FIXTURE),
        ("FAKE_CLAUDE_STDIN_FILE", stdin_file.to_str().unwrap()),
    ]);
    let mut r = req(env.dir.path(), None);
    r.prompt = prompt.to_owned();
    let events = collect_within(&env, r, LINGER_LIMIT).await;
    let stdin = std::fs::read(&stdin_file).unwrap();
    let args = std::fs::read_to_string(env.args_file()).unwrap();
    (events, stdin, args)
}

#[tokio::test]
async fn prompt_goes_to_stdin_even_when_it_looks_like_options() {
    // A markdown list and a diff header would be parsed as options in argv.
    let prompt = "- item\n--- a/x";
    let (events, stdin, args) = turn_with_prompt(prompt).await;
    assert!(
        matches!(events.last(), Some(HarnessEvent::TurnCompleted { .. })),
        "{events:?}"
    );
    assert_eq!(String::from_utf8(stdin).unwrap(), prompt);
    assert!(
        !args.contains("item") && !args.contains("a/x"),
        "the prompt leaked into argv: {args:?}"
    );
}

#[tokio::test]
async fn large_prompt_arrives_intact_on_stdin() {
    // Several hundred KB, far beyond a pipe buffer, with multi-byte characters.
    let prompt: String = (0..12_000)
        .map(|i| format!("line {i}: naïve résumé — {}\n", "x".repeat(20)))
        .collect();
    assert!(prompt.len() > 500 * 1024, "{}", prompt.len());
    let (events, stdin, _) = turn_with_prompt(&prompt).await;
    assert!(
        matches!(events.last(), Some(HarnessEvent::TurnCompleted { .. })),
        "{events:?}"
    );
    assert_eq!(stdin.len(), prompt.len());
    assert!(stdin == prompt.as_bytes(), "the prompt arrived altered");
}

#[tokio::test]
async fn claude_that_never_reads_stdin_does_not_block_the_turn() {
    // This fake-claude ignores stdin: writing a prompt bigger than the pipe
    // buffer fails once it exits, and the turn ends normally.
    let env = setup(&[("FAKE_CLAUDE_FIXTURE", FIXTURE)]);
    let mut r = req(env.dir.path(), None);
    r.prompt = "y".repeat(2 * 1024 * 1024);
    let events = collect_within(&env, r, LINGER_LIMIT).await;
    assert!(
        matches!(events.last(), Some(HarnessEvent::TurnCompleted { .. })),
        "{events:?}"
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, HarnessEvent::Failed { .. })));
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

/// The pid written to `file`, once it is there.
async fn pid_from(file: &Path) -> String {
    let start = Instant::now();
    loop {
        if let Ok(pid) = std::fs::read_to_string(file) {
            if !pid.trim().is_empty() {
                return pid.trim().to_owned();
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{} was never written",
            file.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Fails unless the process is gone within two seconds.
async fn assert_dies(pid: &str, what: &str) {
    let start = Instant::now();
    while pid_alive(pid) {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{what} is still alive"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn dropping_stream_kills_what_claude_started_too() {
    // claude is in the middle of a turn and has started a process of its
    // own (a dev server, say) when the turn is given up.
    let dir = tempfile::tempdir().unwrap();
    let (claude_pid, started_pid) = (dir.path().join("pid"), dir.path().join("started"));
    let env = setup(&[
        ("FAKE_CLAUDE_SLEEP", "30"),
        ("FAKE_CLAUDE_PID_FILE", claude_pid.to_str().unwrap()),
        ("FAKE_CLAUDE_GRANDCHILD_SECS", "30"),
        (
            "FAKE_CLAUDE_GRANDCHILD_PID_FILE",
            started_pid.to_str().unwrap(),
        ),
    ]);
    let stream = env
        .harness
        .run_turn(req(env.dir.path(), None))
        .await
        .unwrap();
    let (claude, started) = (pid_from(&claude_pid).await, pid_from(&started_pid).await);
    assert!(pid_alive(&claude) && pid_alive(&started));

    drop(stream);

    assert_dies(&claude, "claude").await;
    assert_dies(&started, "the process claude started").await;
}

#[tokio::test]
async fn a_turn_given_up_on_asks_claude_to_stop_before_it_is_killed() {
    // Asked first, claude can stop what it started itself, whatever process
    // group that is in.
    let dir = tempfile::tempdir().unwrap();
    let (claude_pid, asked) = (dir.path().join("pid"), dir.path().join("asked"));
    let env = setup(&[
        ("FAKE_CLAUDE_SLEEP", "30"),
        ("FAKE_CLAUDE_PID_FILE", claude_pid.to_str().unwrap()),
        ("FAKE_CLAUDE_TERM_FILE", asked.to_str().unwrap()),
    ]);
    let stream = env
        .harness
        .run_turn(req(env.dir.path(), None))
        .await
        .unwrap();
    let claude = pid_from(&claude_pid).await;

    drop(stream);

    let start = Instant::now();
    while !asked.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "claude was never asked to stop"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_dies(&claude, "claude").await;
}

#[tokio::test]
async fn what_claude_left_running_survives_a_turn_that_ended_on_its_own() {
    // claude exits by itself, leaving a process behind on purpose: the
    // harness only stops what a turn it gave up on had started.
    let dir = tempfile::tempdir().unwrap();
    let started_pid = dir.path().join("started");
    let env = setup(&[
        ("FAKE_CLAUDE_FIXTURE", FIXTURE),
        ("FAKE_CLAUDE_GRANDCHILD_SECS", "60"),
        (
            "FAKE_CLAUDE_GRANDCHILD_PID_FILE",
            started_pid.to_str().unwrap(),
        ),
    ]);

    let events = collect_within(&env, req(env.dir.path(), None), LINGER_LIMIT).await;

    assert!(
        matches!(events.last(), Some(HarnessEvent::TurnCompleted { .. })),
        "{events:?}"
    );
    let started = pid_from(&started_pid).await;
    let survived = pid_alive(&started);
    // The test stops it itself; it outlives any slow run until here.
    let _ = std::process::Command::new("kill").arg(&started).status();
    assert!(survived, "the process claude left was stopped");
}

/// Collects a turn's events, failing instead of hanging past `limit`.
async fn collect_within(env: &Env, r: TurnRequest, limit: Duration) -> Vec<HarnessEvent> {
    tokio::time::timeout(limit, collect(env, r))
        .await
        .unwrap_or_else(|_| panic!("the turn's stream did not end within {limit:?}"))
}

/// Bounds a turn whose process lingers in some way; a hang fails the test.
const LINGER_LIMIT: Duration = Duration::from_secs(15);
/// How soon such a turn must end once claude itself has exited.
const ENDS_WITHIN: Duration = Duration::from_secs(3);

#[tokio::test]
async fn grandchild_holding_stdout_does_not_hide_a_failed_exit() {
    // claude exits 2 without a result line; a process it started keeps stdout open.
    let env = setup(&[
        ("FAKE_CLAUDE_EXIT", "2"),
        ("FAKE_CLAUDE_STDERR", "boom: mcp server crashed"),
        ("FAKE_CLAUDE_GRANDCHILD_STDOUT_SECS", "8"),
    ]);
    let start = Instant::now();
    let events = collect_within(&env, req(env.dir.path(), None), LINGER_LIMIT).await;
    let elapsed = start.elapsed();
    match events.as_slice() {
        [HarnessEvent::Failed { message }] => {
            assert!(message.contains("exit status: 2"), "{message}");
            assert!(message.contains("boom: mcp server crashed"), "{message}");
        }
        other => panic!("expected only Failed, got {other:?}"),
    }
    assert!(elapsed < ENDS_WITHIN, "took {elapsed:?}");
}

#[tokio::test]
async fn grandchild_holding_stdout_keeps_the_final_result_line() {
    // A clean turn whose stdout stays open after claude exits: every line it
    // printed, the result last, still arrives, and the stream then ends.
    let env = setup(&[
        ("FAKE_CLAUDE_FIXTURE", FIXTURE),
        ("FAKE_CLAUDE_GRANDCHILD_STDOUT_SECS", "8"),
    ]);
    let start = Instant::now();
    let events = collect_within(&env, req(env.dir.path(), None), LINGER_LIMIT).await;
    let elapsed = start.elapsed();
    assert!(
        matches!(
            events.last(),
            Some(HarnessEvent::TurnCompleted {
                is_error: false,
                ..
            })
        ),
        "{events:?}"
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, HarnessEvent::Failed { .. })));
    assert!(elapsed < ENDS_WITHIN, "took {elapsed:?}");
}

#[tokio::test]
async fn clean_exit_with_stdout_held_open_just_ends() {
    let env = setup(&[("FAKE_CLAUDE_GRANDCHILD_STDOUT_SECS", "8")]);
    let start = Instant::now();
    let events = collect_within(&env, req(env.dir.path(), None), LINGER_LIMIT).await;
    assert!(events.is_empty(), "{events:?}");
    assert!(start.elapsed() < ENDS_WITHIN, "took {:?}", start.elapsed());
}

#[tokio::test]
async fn stdout_read_error_kills_claude_and_fails() {
    // Output that is not UTF-8 cannot be read as lines; claude, still running,
    // is stopped instead of waited on.
    let dir = tempfile::tempdir().unwrap();
    let garbled = dir.path().join("garbled.jsonl");
    let mut bytes = b"{\"type\":\"bad\xff\xfe\"}\n".to_vec();
    bytes.extend(std::fs::read(FIXTURE).unwrap());
    std::fs::write(&garbled, bytes).unwrap();
    let pid_file = dir.path().join("pid");
    let env = setup(&[
        ("FAKE_CLAUDE_FIXTURE", garbled.to_str().unwrap()),
        ("FAKE_CLAUDE_SLEEP", "8"),
        ("FAKE_CLAUDE_PID_FILE", pid_file.to_str().unwrap()),
    ]);
    let start = Instant::now();
    let events = collect_within(&env, req(env.dir.path(), None), LINGER_LIMIT).await;
    let elapsed = start.elapsed();
    match events.last() {
        Some(HarnessEvent::Failed { message }) => {
            assert!(message.contains("output"), "{message}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(elapsed < ENDS_WITHIN, "took {elapsed:?}");
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    assert!(!pid_alive(pid.trim()), "claude was left running");
}

#[tokio::test]
async fn lingering_grandchild_holding_stderr_does_not_delay_failed() {
    let env = setup(&[
        ("FAKE_CLAUDE_FIXTURE", FIXTURE),
        ("FAKE_CLAUDE_EXIT", "2"),
        ("FAKE_CLAUDE_GRANDCHILD_SECS", "8"),
    ]);
    let start = Instant::now();
    let events = collect(&env, req(env.dir.path(), None)).await;
    assert!(matches!(events.last(), Some(HarnessEvent::Failed { .. })));
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "took {:?}",
        start.elapsed()
    );
}

// ---- run_structured ----

fn sreq(cwd: &Path, prompt: &str, timeout: Duration) -> StructuredRequest {
    StructuredRequest {
        prompt: prompt.into(),
        cwd: cwd.to_path_buf(),
        schema: serde_json::json!({"type":"object"}),
        model: None,
        system_prompt_append: None,
        allowed_tools: vec!["Read".into(), "Grep".into(), "Glob".into()],
        timeout,
    }
}

fn structured_env(extra: &[(&str, &str)]) -> (Env, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    std::fs::write(&out, r#"{"title":"probe","count":2}"#).unwrap();
    let stdin_file = dir.path().join("stdin");
    let mut vars = vec![
        (
            "FAKE_CLAUDE_STRUCTURED_FILE",
            out.to_str().unwrap().to_owned(),
        ),
        (
            "FAKE_CLAUDE_STDIN_FILE",
            stdin_file.to_str().unwrap().to_owned(),
        ),
    ];
    vars.extend(extra.iter().map(|(k, v)| (*k, (*v).to_owned())));
    let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    (setup(&vars), stdin_file, dir)
}

#[tokio::test]
async fn structured_run_passes_flags_and_prompt_on_stdin() {
    let (env, stdin_file, _files) = structured_env(&[]);
    let mut r = sreq(env.dir.path(), "- not an option", Duration::from_secs(10));
    r.model = Some("haiku".into());
    r.system_prompt_append = Some("terse".into());
    let res = env.harness.run_structured(r).await.unwrap();
    assert_eq!(res.value, serde_json::json!({"title":"probe","count":2}));
    assert_eq!(res.cost_usd, Some(0.007));
    let args = std::fs::read_to_string(env.args_file()).unwrap();
    // The exact argv, one entry per line. `-p` is a flag (the prompt goes to
    // stdin); the entry after `--json-schema` is the schema itself; and
    // `--strict-mcp-config` with no `--mcp-config` loads no MCP server, so a
    // polish run can use only the read-only built-in tools it lists.
    let schema = serde_json::json!({"type":"object"}).to_string();
    let argv: Vec<&str> = args.lines().collect();
    assert_eq!(
        argv,
        [
            "-p",
            "--output-format",
            "json",
            "--json-schema",
            schema.as_str(),
            "--dangerously-skip-permissions",
            "--no-session-persistence",
            "--strict-mcp-config",
            "--tools",
            "Read,Grep,Glob",
            "--model",
            "haiku",
            "--append-system-prompt",
            "terse",
        ]
    );
    assert!(!args.contains("not an option"), "{args:?}");
    assert_eq!(
        std::fs::read_to_string(stdin_file).unwrap(),
        "- not an option"
    );
}

#[tokio::test]
async fn structured_timeout_kills_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let (env, _s, _files) = structured_env(&[
        ("FAKE_CLAUDE_SLEEP", "30"),
        ("FAKE_CLAUDE_PID_FILE", pid_file.to_str().unwrap()),
    ]);
    let start = Instant::now();
    let err = env
        .harness
        .run_structured(sreq(env.dir.path(), "x", Duration::from_secs(2)))
        .await
        .unwrap_err();
    assert!(matches!(err, HarnessError::Timeout), "{err:?}");
    assert!(start.elapsed() < Duration::from_secs(5));
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let start = Instant::now();
    while pid_alive(pid.trim()) {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "child still alive"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn structured_nonzero_exit_without_output_is_failed() {
    let env = setup(&[
        ("FAKE_CLAUDE_EXIT", "3"),
        ("FAKE_CLAUDE_STDERR", "bad things"),
    ]);
    let err = env
        .harness
        .run_structured(sreq(env.dir.path(), "x", Duration::from_secs(10)))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, HarnessError::Failed(m) if m.contains("bad things")),
        "{err:?}"
    );
}

#[tokio::test]
async fn structured_clean_exit_without_output_is_invalid() {
    let env = setup(&[]);
    let err = env
        .harness
        .run_structured(sreq(env.dir.path(), "x", Duration::from_secs(10)))
        .await
        .unwrap_err();
    assert!(matches!(err, HarnessError::InvalidOutput(_)), "{err:?}");
}
